//! Buffers, replays, broadcasts, and atomically closes downstream output.

use bytes::Bytes;
use tokio::sync::mpsc;

#[derive(Default)]
pub struct OutputHub {
    state: parking_lot::Mutex<OutputState>,
}

#[derive(Default)]
struct OutputState {
    history: Vec<Bytes>,
    subscribers: Vec<mpsc::UnboundedSender<Bytes>>,
    closed: bool,
    http_active: usize,
    http_epoch: u64,
    http_sealed: bool,
}

impl OutputHub {
    pub fn emit(&self, frame: Bytes) -> bool {
        let mut state = self.state.lock();
        if state.closed {
            return false;
        }
        state.history.push(frame.clone());
        state
            .subscribers
            .retain(|subscriber| subscriber.send(frame.clone()).is_ok());
        true
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Bytes> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock();
        for frame in &state.history {
            let _ = sender.send(frame.clone());
        }
        if !state.closed && !state.http_sealed {
            state.subscribers.push(sender);
        }
        receiver
    }

    pub(crate) fn acquire_http(&self) -> bool {
        let mut state = self.state.lock();
        if state.closed || state.http_sealed {
            return false;
        }
        state.http_active += 1;
        state.http_epoch = state.http_epoch.wrapping_add(1);
        true
    }
    pub(crate) fn release_http(&self) -> Option<u64> {
        let mut state = self.state.lock();
        state.http_active -= 1;
        (state.http_active == 0 && !state.closed).then_some(state.http_epoch)
    }
    pub(crate) fn disconnect_if_unsubscribed(&self, epoch: u64) -> bool {
        let mut state = self.state.lock();
        if state.closed || state.http_active != 0 || state.http_epoch != epoch {
            return false;
        }
        state.http_sealed = true;
        true
    }

    pub fn close(&self) -> bool {
        let mut state = self.state.lock();
        if state.closed {
            return false;
        }
        state.closed = true;
        state.subscribers.clear();
        drop(state);
        true
    }
}
