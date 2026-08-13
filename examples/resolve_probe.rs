//! Probes what `symbology.resolve` will and will not enumerate.
//!
//! Three questions, none of which the type signatures answer:
//!
//! 1. Does `resolve` honour `ALL_SYMBOLS`, or reject it? `ResolveParams::symbols` takes
//!    anything convertible into `Symbols`, so `Symbols::All` compiles either way.
//! 2. Does a `parent` selection enumerate its family?
//! 3. What does a symbol that expires mid-range land in - `mappings` or `partial`?
//!
//! Resolve is a metadata endpoint, so every call here is free. Run with:
//!
//! ```sh
//! brokkr run resolve_probe
//! ```

use anyhow::{Context, Result};
use databento::{
    HistoricalClient, Symbols,
    dbn::SType,
    historical::{DateRange, symbology::ResolveParams},
};
use time::macros::date;

#[tokio::main]
async fn main() -> Result<()> {
    let key = std::fs::read_to_string("databento.key").context("reading databento.key")?;
    let mut client = HistoricalClient::builder()
        .key(key.trim())?
        .build()
        .context("building client")?;

    // 1. ALL_SYMBOLS over a single day.
    probe(
        &mut client,
        "ALL_SYMBOLS, raw_symbol -> instrument_id",
        Symbols::All,
        SType::RawSymbol,
        SType::InstrumentId,
    )
    .await;

    // 2. A parent selection, which should expand to the family.
    probe(
        &mut client,
        "ES.FUT, parent -> raw_symbol",
        Symbols::Symbols(vec!["ES.FUT".to_owned()]),
        SType::Parent,
        SType::RawSymbol,
    )
    .await;

    // 2b. The same parent selection, resolved to ids instead of raw symbols.
    probe(
        &mut client,
        "ES.FUT, parent -> instrument_id",
        Symbols::Symbols(vec!["ES.FUT".to_owned()]),
        SType::Parent,
        SType::InstrumentId,
    )
    .await;

    // 2c. A continuous contract, which has to expand to the underlying contracts.
    probe(
        &mut client,
        "ES.v.0, continuous -> raw_symbol",
        Symbols::Symbols(vec!["ES.v.0".to_owned()]),
        SType::Continuous,
        SType::RawSymbol,
    )
    .await;

    // 2d. Can the parent symbols themselves be enumerated?
    probe(
        &mut client,
        "ALL_SYMBOLS, parent -> instrument_id",
        Symbols::All,
        SType::Parent,
        SType::InstrumentId,
    )
    .await;

    // 3. A contract that expires inside the range.
    probe(
        &mut client,
        "ESM2, raw_symbol -> instrument_id",
        Symbols::Symbols(vec!["ESM2".to_owned()]),
        SType::RawSymbol,
        SType::InstrumentId,
    )
    .await;

    Ok(())
}

/// Reports the shape of one resolution rather than dumping it: these responses run to
/// thousands of entries.
async fn probe(
    client: &mut HistoricalClient,
    label: &str,
    symbols: Symbols,
    stype_in: SType,
    stype_out: SType,
) {
    println!("== {label}");
    let params = ResolveParams::builder()
        .dataset("GLBX.MDP3")
        .symbols(symbols)
        .stype_in(stype_in)
        .stype_out(stype_out)
        .date_range(DateRange::from(
            date!(2022 - 06 - 01)..date!(2022 - 06 - 30),
        ))
        .build();

    match client.symbology().resolve(&params).await {
        Ok(res) => {
            println!(
                "   mapped {}, partial {}, not_found {}",
                res.mappings.len(),
                res.partial.len(),
                res.not_found.len()
            );
            let mut keys: Vec<&String> = res.mappings.keys().collect();
            keys.sort();
            for key in keys.iter().take(5) {
                let intervals = &res.mappings[*key];
                let sample = intervals
                    .first()
                    .map_or_else(String::new, |i| format!("{} -> {}", i.start_date, i.symbol));
                println!("   {key}: {} interval(s), {sample}", intervals.len());
            }
            if keys.len() > 5 {
                println!("   ... and {} more", keys.len() - 5);
            }
            for name in res.partial.iter().take(5) {
                println!("   partial: {name}");
            }
            for name in res.not_found.iter().take(5) {
                println!("   not_found: {name}");
            }
        }
        Err(err) => println!("   REFUSED: {err}"),
    }
    println!();
}
