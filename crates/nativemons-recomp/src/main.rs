//! nativemons-recomp build <rom> <work dir> <library> [--cache <dir>] [--shared-hints <dir>] | check <library> |
//! id <rom>: recompiles a player's own dump for Zakuro with no window, for the nativeMons launcher.

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
            let option = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(PathBuf::from);
            let rom = Path::new(&args[1]);
            match option("--shared-hints") {
                Some(dir) => merge_shared_hints(rom, &dir),
                None => Ok(()),
            }
            .and_then(|()| run_build(rom, Path::new(&args[2]), Path::new(&args[3]), option("--cache").as_deref()))
        }
        Some("check") if args.len() == 2 => return check(Path::new(&args[1])),
        Some("id") if args.len() == 2 => title_id(Path::new(&args[1])).map(|(id, hash)| report(&format!("id {id:016X} {hash:016X}"))),
        _ => Err("usage: nativemons-recomp build <rom> <work dir> <library> [--cache <dir>] [--shared-hints <dir>] | check <library> | id <rom>".to_owned()),
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

/// the title id and the hash 3dsrecomp keys hints by: one revision or update's addresses never reach another's.
fn title_id(rom: &Path) -> Result<(u64, u64), String> {
    let title = recomp3ds::rom::Title::load(rom).map_err(|e| format!("could not load {}, {e}", rom.display()))?;
    let programs = recomp3ds::programs(&title, recomp3ds::Mods::default()).map_err(|e| format!("could not read the code, {e}"))?;
    let (_, exe) = programs.iter().find(|(name, _)| name == "executable").ok_or("the title has no executable")?;
    Ok((title.program_id(), build::code_hash(exe)))
}

/// adds the shared list for this exact code (<dir>/<title id>-<code hash>.hints, from players who shared theirs) to
/// the player's own in 3dsrecomp's folder, which the build reads. 3dsrecomp keeps only addresses inside the code.
fn merge_shared_hints(rom: &Path, dir: &Path) -> Result<(), String> {
    let (id, hash) = title_id(rom)?;
    let shared = dir.join(format!("{id:016X}-{hash:016X}.hints"));
    let Ok(text) = std::fs::read_to_string(&shared) else { return Ok(()) };
    let Some(own_dir) = recomp_abi::library_dir() else { return Ok(()) };
    let own = own_dir.join(recomp_abi::library_name(id)).with_extension("hints");
    let mut all: Vec<u32> = build::read_hints(&std::fs::read_to_string(&own).unwrap_or_default());
    let before = all.len();
    all.extend(build::read_hints(&text));
    all.sort_unstable();
    all.dedup();
    std::fs::create_dir_all(&own_dir).map_err(|e| format!("could not create {}, {e}", own_dir.display()))?;
    let body: String = all.iter().map(|a| format!("{a:08X}\n")).collect();
    std::fs::write(&own, format!("# this player's and shared hints, odd for Thumb\n{body}")).map_err(|e| format!("could not write {}, {e}", own.display()))?;
    report(&format!("1 {} shared hints added to {before}", all.len() - before));
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
