//! The run loop: terminal events, engine events (micro-batched), async
//! command results and a tick, all feeding `App::update`, then a redraw when
//! something changed.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::EventStream;
use futures::StreamExt;
use lz_schema::api::EngineApi;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::{App, Msg, Suspend, TuiOptions};
use crate::terminal::{Guard, install_panic_hook, query_colors};

pub async fn run(api: Arc<dyn EngineApi>, opts: TuiOptions) -> anyhow::Result<()> {
    install_panic_hook();
    let mouse = opts.tui.mouse.unwrap_or(true);
    let system_colors = query_colors(Duration::from_millis(120));
    let guard = Guard::enter(mouse)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let mut app = App::new(api.clone(), tx.clone(), opts, system_colors);
    app.bootstrap();

    // engine events → 16ms micro-batches
    {
        let tx = tx.clone();
        let mut stream = api.subscribe();
        tokio::spawn(async move {
            let mut batch = Vec::new();
            loop {
                tokio::select! {
                    ev = stream.next() => {
                        match ev {
                            Some(e) => batch.push(e),
                            None => break,
                        }
                        // drain what's immediately available, then wait ≤16ms for more
                        let deadline = tokio::time::sleep(Duration::from_millis(16));
                        tokio::pin!(deadline);
                        loop {
                            tokio::select! {
                                more = stream.next() => match more {
                                    Some(e) => batch.push(e),
                                    None => break,
                                },
                                _ = &mut deadline => break,
                            }
                            if batch.len() > 256 { break; }
                        }
                        if tx.send(Msg::Engine(std::mem::take(&mut batch))).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    // tick
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(100));
            loop {
                iv.tick().await;
                if tx.send(Msg::Tick).is_err() {
                    break;
                }
            }
        });
    }

    let mut events = Some(EventStream::new());
    crate::terminal::set_title("LZ");
    terminal.draw(|f| app.view(f))?;
    app.dirty = false;

    loop {
        let msg = {
            let stream = events.as_mut().expect("event stream");
            tokio::select! {
                m = rx.recv() => match m { Some(m) => m, None => break },
                ev = stream.next() => match ev {
                    Some(Ok(ev)) => Msg::Term(ev),
                    Some(Err(e)) => { tracing::warn!("terminal event error: {e}"); continue; }
                    None => break,
                },
            }
        };
        app.update(msg);
        // coalesce anything already queued before drawing
        while let Ok(m) = rx.try_recv() {
            app.update(m);
        }
        if app.quit {
            break;
        }
        if let Some(s) = app.suspend.take() {
            drop(events.take()); // release stdin
            match s {
                Suspend::Editor(initial) => {
                    let r = guard.suspend(|| crate::editor::edit_blocking(&initial));
                    match r {
                        Ok(Ok(text)) => app.update(Msg::EditorDone(text)),
                        Ok(Err(e)) => app.update(Msg::Error(format!("editor: {e}"))),
                        Err(e) => app.update(Msg::Error(format!("terminal: {e}"))),
                    }
                }
                Suspend::Stop => {
                    // ctrl+z: hand the terminal back to the shell (no job control on Windows)
                    #[cfg(unix)]
                    {
                        let _ = guard.suspend(|| unsafe {
                            libc::kill(0, libc::SIGTSTP);
                        });
                    }
                    #[cfg(not(unix))]
                    {
                        app.update(Msg::Error("suspend is not available on this platform".into()));
                    }
                }
            }
            events = Some(EventStream::new());
            // fresh buffers: the alternate screen is blank again after re-entering
            terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
            app.dirty = true;
        }
        if app.dirty {
            terminal.draw(|f| app.view(f))?;
            app.dirty = false;
        }
    }
    drop(guard);
    Ok(())
}
