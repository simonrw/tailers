use chrono::prelude::*;
use notify::{op::Op, raw_watcher, RawEvent, RecursiveMode, Watcher};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use structopt::StructOpt;
use tracing::{event, Level};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, StructOpt)]
#[structopt(name = "tailers", version = "0.1.0")]
struct Opt {
    #[structopt(short, long)]
    files: Vec<String>,
    // TODO: docker
    // TODO: kubernetes
}

#[derive(Debug)]
struct LogEvent {
    filename: PathBuf,
    line: String,
}

struct LogFileTailer {
    // Sender for the outer channel which broadcasts log messages
    sender: Arc<Mutex<mpsc::Sender<LogEvent>>>,

    // Receiver for the notifications
    rx: Arc<Mutex<mpsc::Receiver<RawEvent>>>,
    // TODO: make this generic over this parameter to support other operating systems
    _watcher: notify::inotify::INotifyWatcher,

    reader: Arc<Mutex<BufReader<File>>>,
}

impl LogFileTailer {
    fn new<P>(path: P, sender: mpsc::Sender<LogEvent>) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let (tx, rx) = mpsc::channel();
        // TODO: single shared watcher for the whole crate?
        let mut watcher = raw_watcher(tx)?;
        watcher.watch(&path, RecursiveMode::NonRecursive)?;

        // Get the last seek position of the file to continue from
        let mut f = File::open(&path)?;
        f.seek(SeekFrom::End(0))?;
        let reader = BufReader::new(f);

        Ok(Self {
            sender: Arc::new(Mutex::new(sender)),
            rx: Arc::new(Mutex::new(rx)),
            _watcher: watcher,
            reader: Arc::new(Mutex::new(reader)),
        })
    }

    fn start(&mut self) {
        let rx = self.rx.clone();
        let sender = self.sender.clone();
        let reader = self.reader.clone();

        thread::spawn(move || loop {
            let rx = rx.lock().unwrap();
            match rx.recv() {
                Ok(RawEvent {
                    path: Some(path),
                    op: Ok(Op::WRITE),
                    cookie: _cookie,
                }) => {
                    let sender = sender.lock().unwrap();
                    let mut reader = reader.lock().unwrap();

                    let mut buf = String::new();
                    loop {
                        match reader.read_line(&mut buf) {
                            // we have reached the end of the file
                            Ok(0) => break,
                            Ok(_) => sender
                                .send(LogEvent {
                                    filename: path.clone(),
                                    line: buf.trim_end().to_string(),
                                })
                                .unwrap(),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Ok(_) => {
                    // Some other event
                    // TODO: handle file renaming or deletion
                }
                Err(err) => {
                    eprintln!("error: {:?}", err);
                    break Ok(());
                }
            }
        });
    }
}

trait Tailer {}

impl Tailer for LogFileTailer {}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    event!(Level::INFO, "Program starting");

    let opts = Opt::from_args();

    // Message channel for receiving new messages from all tailers
    let (tx, rx) = mpsc::channel();

    // Keep track of all tailers so that their channels do not get closed
    let mut tailers: Vec<Box<dyn Tailer>> = Vec::new();

    // Start by adding the files requested
    for file in opts.files {
        event!(Level::INFO, ?file, "adding file");
        let mut tailer = LogFileTailer::new(file, tx.clone()).unwrap();
        tailer.start();
        tailers.push(Box::new(tailer));
    }

    event!(Level::INFO, "watching {} sources", tailers.len());

    loop {
        let event = rx.recv()?;
        if let Some(p) = event.filename.to_str() {
            println!("{}: {}", p, event.line);
        }
    }
}
