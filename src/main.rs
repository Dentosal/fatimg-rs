#![deny(unused_must_use)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use fatfs::{format_volume, Dir, FileSystem, FormatVolumeOptions, FsOptions};
use fatfs::{StdIoWrapper, Write};
use fscommon::BufStream;

/// FAT filesystem image manipulation tool
#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// Operation
    #[clap(subcommand)]
    cmd: Command,
    /// File to operate on
    #[clap(parse(from_os_str))]
    img_file: PathBuf,
}

#[derive(Parser, Debug)]
enum Command {
    /// Create a new filesystem
    Create {
        /// Size of the created image, in bytes
        #[clap(short, long)]
        size: u64,
        /// Overwrite existing output file
        #[clap(short, long)]
        force: bool,
    },
    /// Read filesystem info
    Info,
    /// List directory contents
    Ls {
        /// Path in the image
        #[clap(default_value = "/")]
        inner_path: String,

        /// List file attributes as well, like `ls -l`.
        /// Give multiple times to show even more info.
        #[clap(short, long, parse(from_occurrences), display_order = 1)]
        long: u8,

        /// List subdirectory contents recursively, like `tree`
        #[clap(short, long)]
        recursive: bool,
    },
    /// Create a directory
    Mkdir {
        /// Path in the image
        inner_path: String,
    },
    /// Read a file
    Read {
        /// Path in the image
        inner_path: String,
    },
    /// Read a file
    Write {
        /// Path in the image
        inner_path: String,

        /// Write contents of this file. Stdin is used if not specified.
        /// The file is overwritten is it exists.
        #[clap(short = 'i', long = "--input", parse(from_os_str))]
        host_path: Option<PathBuf>,
    },
    /// Read filesystem tree into host fs
    ReadTree {
        /// Path in the image
        #[clap(short = 's', long = "--subtree", default_value = "/")]
        inner_path: String,

        /// Path in the image
        #[clap(parse(from_os_str))]
        host_path: PathBuf,

        /// Overwrite existing output
        #[clap(short, long)]
        force: bool,
    },
    /// Write filesystem tree from host fs.
    /// The tree is overwritten is it exists.
    WriteTree {
        /// Path in the image
        #[clap(short = 's', long = "--subtree", default_value = "/")]
        inner_path: String,

        /// Path in the image
        #[clap(parse(from_os_str))]
        host_path: PathBuf,
    },
}

fn normalize_inner_path(p: String) -> String {
    let p = p.strip_prefix("/").expect("Absolute path required");

    let mut result = Vec::new();
    for s in p.split("/") {
        if !s.is_empty() {
            result.push(s);
        }
    }
    result.join("/")
}

fn print_date(date: fatfs::Date) {
    print!("{:04}-{:02}-{:02}", date.year, date.month, date.day,)
}

fn print_time(time: fatfs::Time) {
    print!(
        "{:02}:{:02}:{:02}.{:03}",
        time.hour, time.min, time.sec, time.millis
    )
}

fn print_datetime(dt: fatfs::DateTime) {
    print_date(dt.date);
    print!(" ");
    print_time(dt.time);
}

fn print_ls<'a, IO: fatfs::ReadWriteSeek, TP: fatfs::TimeProvider, OCC: fatfs::OemCpConverter>(
    cursor: Dir<'a, IO, TP, OCC>, long: u8, recursive: bool, indent: usize,
) -> Result<()> {
    let indent_str = "  ".repeat(indent);
    for entry in cursor.iter() {
        let entry = entry.expect("Dir entry");
        let name = entry.file_name();

        if name == "." || name == ".." {
            continue;
        }

        print!("{}", indent_str);

        if long >= 3 {
            print!("created ");
            print_datetime(entry.created());
            print!(" ");
        }
        if long >= 2 {
            print!("modified ");
            print_datetime(entry.modified());
            print!(" ");
        }
        if long >= 3 {
            print!("accessed ");
            print_date(entry.accessed());
            print!(" ");
        }

        if long >= 1 {
            print!("{:?} ", entry.attributes());

            if entry.is_file() {
                print!("size {} ", entry.len());
            }
        }

        println!("{}{}", name, if entry.is_dir() { "/" } else { "" });

        if recursive && entry.is_dir() {
            print_ls(entry.to_dir(), long, recursive, indent + 1)?;
        }
    }
    Ok(())
}

fn write_tree_to_img<
    'a,
    IO: fatfs::ReadWriteSeek,
    TP: fatfs::TimeProvider,
    OCC: fatfs::OemCpConverter,
>(
    cursor: Dir<'a, IO, TP, OCC>, host_path: PathBuf,
) -> Result<()> {
    for entry in cursor.iter() {
        let entry = entry.expect("Entry");
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        todo!("Ovewrite non-empty directory?");
    }

    for entry in fs::read_dir(host_path)? {
        let entry = entry?;
        let t = entry.file_type()?;
        let name = entry.file_name().into_string().expect("non-utf8 filename");

        if t.is_symlink() {
            eprintln!("Warning: Not copying a symlink");
        }

        if t.is_file() {
            let source_file = File::open(entry.path())?;
            let mut source = io::BufReader::new(source_file);
            let mut target_file = cursor.create_file(&name).expect("Create file");

            let mut buf = [0u8; 1024];
            loop {
                let n = source.read(&mut buf)?;
                target_file.write(&buf[..n]).expect("Write");
                if n == 0 {
                    break;
                }
            }

            // let t = StdIoWrapper::from(target_file);
            // io::copy(&mut source, &mut t)?;
        }

        if t.is_dir() {
            let subdir = cursor.create_dir(&name).expect("Dir entry");
            write_tree_to_img(subdir, entry.path())?;
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    env_logger::init();

    let args = Args::parse();
    println!("{:?}", args);

    match args.cmd {
        Command::Create { force, size } => {
            let img_file = if force {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(args.img_file)?
            } else {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(args.img_file)?
            };

            img_file.set_len(size)?;
            let buf_file = BufStream::new(img_file);
            format_volume(
                &mut StdIoWrapper::from(buf_file),
                FormatVolumeOptions::new(),
            )?;
            Ok(())
        },
        Command::Info => {
            let img_file = File::open(args.img_file)?;
            let buf_file = BufStream::new(img_file);
            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            println!("fs type:       {:?}", fs.fat_type());
            println!("volume id:     {:?}", fs.volume_id());
            println!("volume label:  {:?}", fs.volume_label());
            let stats = fs.stats()?;
            println!("cluster size:  {:?}", stats.cluster_size());
            let ct = stats.total_clusters();
            let cf = stats.free_clusters();
            println!("cluster count: {:?}", ct);
            println!("clusters free: {:?}", cf);
            println!("usage:         {:?}%", ((ct - cf) * 100) / ct);
            Ok(())
        },
        Command::Ls {
            inner_path,
            long,
            recursive,
        } => {
            let inner_path = normalize_inner_path(inner_path);
            let img_file = File::open(args.img_file)?;
            let buf_file = BufStream::new(img_file);
            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            let mut cursor = fs.root_dir();
            if !inner_path.is_empty() {
                cursor = cursor.open_dir(&inner_path)?;
            }

            print_ls(cursor, long, recursive, 0)
        },
        Command::Mkdir { inner_path } => {
            let inner_path = normalize_inner_path(inner_path);
            let img_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(false)
                .open(args.img_file)?;
            let buf_file = BufStream::new(img_file);
            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            fs.root_dir().create_dir(&inner_path)?;
            Ok(())
        },
        Command::Read { inner_path } => {
            let inner_path = normalize_inner_path(inner_path);

            let img_file = OpenOptions::new()
                .read(true)
                .write(false)
                .create(false)
                .open(args.img_file)?;
            let buf_file = BufStream::new(img_file);

            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            let mut source = fs.root_dir().open_file(&inner_path)?;

            io::copy(&mut source, &mut io::stdout())?;

            Ok(())
        },
        Command::Write {
            inner_path,
            host_path,
        } => {
            let inner_path = normalize_inner_path(inner_path);

            let img_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(false)
                .open(args.img_file)?;
            let buf_file = BufStream::new(img_file);

            let mut source: Box<dyn io::BufRead> = if let Some(p) = host_path {
                let source_file = File::open(p)?;
                Box::new(io::BufReader::new(source_file))
            } else {
                Box::new(io::BufReader::new(io::stdin()))
            };

            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            let mut target_file = fs.root_dir().create_file(&inner_path)?;
            target_file.truncate()?;

            io::copy(&mut source, &mut target_file)?;

            Ok(())
        },
        Command::ReadTree {
            inner_path,
            host_path,
            force,
        } => {
            let inner_path = normalize_inner_path(inner_path);

            let img_file = OpenOptions::new()
                .read(true)
                .write(false)
                .create(false)
                .open(args.img_file)?;
            let buf_file = BufStream::new(img_file);

            todo!("ReadTree");
        },
        Command::WriteTree {
            inner_path,
            host_path,
        } => {
            let inner_path = normalize_inner_path(inner_path);

            let img_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(false)
                .open(args.img_file)?;
            let buf_file = BufStream::new(img_file);
            let fs = FileSystem::new(buf_file, FsOptions::new())?;
            let mut cursor = fs.root_dir();
            if !inner_path.is_empty() {
                cursor = cursor.open_dir(&inner_path)?;
            }
            write_tree_to_img(cursor, host_path)
        },
    }
}
