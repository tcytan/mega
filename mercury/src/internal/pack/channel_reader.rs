use std::io;
use std::io::{BufRead, Read};
use std::sync::mpsc::Receiver;

/// Custom BufRead implementation that reads from the channel
pub(crate) struct ChannelReader {
    receiver: Receiver<Vec<u8>>,
    buffer: io::Cursor<Vec<u8>>,
}

impl ChannelReader {
    pub(crate) fn new(receiver: Receiver<Vec<u8>>) -> Self {
        ChannelReader {
            receiver,
            buffer: io::Cursor::new(Vec::new()),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.buffer.position() as usize == self.buffer.get_ref().len() { // buffer has been read completely
            match self.receiver.recv() {
                Ok(data) => {
                    self.buffer = io::Cursor::new(data);
                }
                Err(_) => return Ok(0), // Channel is closed
            }
        }
        self.buffer.read(buf)
    }
}

impl BufRead for ChannelReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buffer.position() as usize == self.buffer.get_ref().len() {
            match self.receiver.recv() {
                Ok(data) => {
                    self.buffer = io::Cursor::new(data);
                }
                Err(_) => return Ok(&[]), // Channel is closed
            }
        }
        self.buffer.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.buffer.consume(amt);
    }
}