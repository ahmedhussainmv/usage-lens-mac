use std::path::PathBuf;
use usage_lens_mac::{
    collector::{collect, Roots},
    store::{open, query, Filter},
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--collect") {
        let home = PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?,
        );
        let db = args
            .iter()
            .position(|a| a == "--database")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home.join("Library/Application Support/Usage Lens Mac/usage.sqlite")
            });
        let scan = collect(&Roots::for_home(&home), &db)?;
        println!("Scan: {scan:?}");
        for source in ["codex", "claude"] {
            let s = query(
                &open(&db)?,
                &Filter {
                    source: source.into(),
                    since: None,
                    ..Filter::default()
                },
            )?;
            println!("{source}: responses/events={} threads={} uncached={} cache_read={} cache_write={} output={} incomplete={} undated={} excluded={} conflicting_turns={}",s.events,s.threads,s.uncached,s.cache_read,s.cache_write,s.output,s.incomplete,s.undated,s.excluded,s.conflicting_turns);
        }
    } else {
        if args.iter().any(|a| a == "--demo") {
            std::env::set_var("USAGE_LENS_DEMO", "1");
        }
        #[cfg(target_os = "macos")]
        usage_lens_mac::ui::run()?;
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("The native interface currently requires macOS.");
    }
    Ok(())
}
