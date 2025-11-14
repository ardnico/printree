use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
#[allow(dead_code)]
pub struct Frame {
    pub path: PathBuf,
    pub entries: Vec<fs::DirEntry>,
    pub idx: usize,
    pub prefix: String,
    pub depth: usize,
}
