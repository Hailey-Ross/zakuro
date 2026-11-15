//! nativemons-recomp build <rom> <work dir> <library> [--cache <dir>] | check <library>: recompiles a player's own
//! dump for Zakuro with no window, printing "nativemons: <percent> <text>" lines for the nativeMons launcher.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use recomp3ds::build::{self, Event};
use recomp3ds::compile;
use sha2::{Digest, Sha256};

fn report(text: &str) {
    println!("nativemons: {text}");
    let _ = std::io::stdout().flush();
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build") if args.len() >= 4 => {
            let cache = args.iter().position(|a| a == "--cache").and_then(|i| args.get(i + 1)).map(PathBuf::from);
            run_build(Path::new(&args[1]), Path::new(&args[2]), Path::new(&args[3]), cache.as_deref())
        }
        Some("check") if args.len() == 2 => return check(Path::new(&args[1])),
        _ => Err("usage: nativemons-recomp build <rom> <work dir> <library> [--cache <dir>] | check <library>".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&format!("error {error}"));
            ExitCode::from(2)
        }
    }
}

/// "ok" when this Zakuro runs the library as it is, "old" when it runs but a rebuild would be better, and
/// "outdated" when it would interpret everything instead.
fn check(library: &Path) -> ExitCode {
    match recomp_abi::Library::open(library) {
        Ok(lib) if lib.generation() >= recomp_abi::GENERATION => {
            report("ok");
            ExitCode::SUCCESS
        }
        Ok(lib) => {
            report(&format!("old generation {} of {}", lib.generation(), recomp_abi::GENERATION));
            ExitCode::from(4)
        }
        Err(error) => {
            report(&format!("outdated {error}"));
            ExitCode::from(3)
        }
    }
}

fn run_build(rom: &Path, work: &Path, library: &Path, cache: Option<&Path>) -> Result<(), String> {
    let compiler = compile::check(None)?;
    report("1 Reading the game");
    let events = |event: Event| {
        if let Event::Generated { bytes, .. } = event {
            report(&format!("4 Wrote {} MB of C", bytes / (1 << 20)));
        }
    };
    let generated = build::generate(rom, |_| work.to_owned(), None, recomp3ds::Mods::default(), &events)?;
    let dir = &generated.dir;
    let total = generated.sources.len();

    let id = format!(
        "nativemons-recomp 1\0{}\0{}\0abi {} gen {}\0",
        compiler.describe(),
        std::env::var("NATIVEMONS_RECOMP_COMPILER").unwrap_or_default(),
        recomp_abi::ABI,
        recomp_abi::GENERATION
    );
    let mut misses = Vec::new();
    let mut keys = Vec::new();
    for source in &generated.sources {
        let key = cache.map(|_| key_of(&id, dir, source)).transpose()?;
        let hit = match (cache, &key) {
            (Some(cache), Some(key)) => {
                let cached = cache.join(format!("{key}.o"));
                cached.is_file() && std::fs::copy(&cached, dir.join(source).with_extension("o")).is_ok()
            }
            _ => false,
        };
        if !hit {
            misses.push(source.clone());
        }
        keys.push(key);
    }
    let hits = total - misses.len();
    report(&format!("5 {hits} of {total} parts from the cache"));

    let progress = |done: usize, _: usize| {
        let pct = 5 + (hits + done) * 90 / total.max(1);
        report(&format!("{pct} Compiled {} of {total}", hits + done));
        true
    };
    compile::objects(&compiler, dir, &misses, &progress, false)?;

    if let Some(cache) = cache {
        std::fs::create_dir_all(cache).map_err(|e| format!("could not create {}, {e}", cache.display()))?;
        let missed: HashSet<&String> = misses.iter().collect();
        for (source, key) in generated.sources.iter().zip(&keys) {
            let (true, Some(key)) = (missed.contains(source), key) else { continue };
            let target = cache.join(format!("{key}.o"));
            let temp = cache.join(format!("{key}.o.part"));
            if std::fs::copy(dir.join(source).with_extension("o"), &temp).is_ok() {
                let _ = std::fs::rename(&temp, &target);
            }
        }
    }

    report("96 Linking");
    let objects: Vec<PathBuf> = generated.sources.iter().map(|s| dir.join(s).with_extension("o")).collect();
    if let Some(parent) = library.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}, {e}", parent.display()))?;
    }
    let temp = library.with_extension("part");
    compile::shared(&compiler, &objects, &temp)?;
    std::fs::rename(&temp, library).map_err(|e| format!("could not finish {}, {e}", library.display()))?;
    report("100 Ready");
    Ok(())
}

/// a hash of what decides the object: the compiler, the C and every header it includes from the work dir.
fn key_of(id: &str, dir: &Path, source: &str) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(id.as_bytes());
    let mut seen = HashSet::new();
    let mut todo = vec![source.to_owned()];
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let path = dir.join(&name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        for line in bytes.split(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(line);
            if let Some(rest) = line.trim_start().strip_prefix("#include \"") {
                if let Some(end) = rest.find('"') {
                    todo.push(rest[..end].to_owned());
                }
            }
        }
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
