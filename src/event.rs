//! Terminal event pump.
//!
//! One thread reads crossterm events and also produces a tick when nothing
//! arrives, so the main loop has a single blocking source of input. Nothing
//! else in the app owns a timer.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEvent, KeyEventKind};

#[derive(Debug)]
pub enum Input {
    Key(KeyEvent),
    Resize,
    Tick,
}

pub struct Events {
    rx: Receiver<Input>,
}

impl Events {
    pub fn spawn(tick: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("input".to_string())
            .spawn(move || loop {
                let ready = event::poll(tick).unwrap_or(false);
                if ready {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            // Ignore key releases: they would double every action.
                            if key.kind == KeyEventKind::Press && tx.send(Input::Key(key)).is_err()
                            {
                                break;
                            }
                        }
                        Ok(Event::Resize(_, _)) => {
                            if tx.send(Input::Resize).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                } else if tx.send(Input::Tick).is_err() {
                    break;
                }
            })
            .expect("spawn input thread");
        Self { rx }
    }

    /// Block until the next input or tick.
    pub fn next(&self) -> Option<Input> {
        self.rx.recv().ok()
    }
}
