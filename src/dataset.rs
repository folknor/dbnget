//! `dbnget dataset` - everything about one dataset, and the dataset listing.

use anyhow::{Context, Result, bail};
use databento::{HistoricalClient, dbn::Schema, historical::metadata::FeedMode};
use tracing::debug;

use crate::{Outcome, cli::DatasetArgs};

/// `dbnget list datasets` - one dataset code per line, only what the account can
/// access. This is where `--dataset` values come from.
pub async fn list(client: &mut HistoricalClient) -> Result<Outcome> {
    let datasets = client
        .metadata()
        .list_datasets(None)
        .await
        .context("listing datasets")?;
    for dataset in &datasets {
        println!("{dataset}");
    }
    Ok(Outcome::Settled)
}

pub async fn run(client: &mut HistoricalClient, args: &DatasetArgs) -> Result<Outcome> {
    if args.publishers {
        publishers(client, &args.dataset).await?;
    } else {
        card(client, &args.dataset).await?;
    }
    Ok(Outcome::Settled)
}

/// The dataset card: its available range and its schemas, the two facts needed to
/// turn a dataset code into a fetch command.
async fn card(client: &mut HistoricalClient, dataset: &str) -> Result<()> {
    let range = client
        .metadata()
        .get_dataset_range(dataset)
        .await
        .with_context(|| format!("fetching range for {dataset}"))?;
    println!("range:   {} .. {}", range.start, range.end);

    let schemas = client
        .metadata()
        .list_schemas(dataset)
        .await
        .with_context(|| format!("listing schemas for {dataset}"))?;
    let names: Vec<String> = schemas.iter().map(ToString::to_string).collect();
    println!("schemas: {}", names.join(" "));

    prices(client, dataset, &schemas).await;
    Ok(())
}

/// What each schema costs per unit on this dataset, for the historical feed mode.
///
/// This is the only thing that makes two datasets carrying the same symbol comparable
/// before you buy either. `ohlcv-1m` is $12.00 per unit on EQUS.MINI and $35.00 on
/// DBEQ.BASIC; nothing else dbnget prints hints at that, and finding it by pricing both
/// on a hunch is not discovery.
///
/// The batch and streaming paths share the historical rate, so one row is the whole
/// story for anything dbnget can do. The live mode the endpoint also returns is not a
/// mode this tool fetches in, and printing it would only invite the comparison.
///
/// Advisory, like the symbology summary: the dataset card is worth printing without it,
/// so a failure here is logged and the card stands.
async fn prices(client: &mut HistoricalClient, dataset: &str, schemas: &[Schema]) {
    let modes = match client.metadata().list_unit_prices(dataset).await {
        Ok(modes) => modes,
        Err(err) => {
            debug!(%err, "unit prices unavailable for this dataset");
            return;
        }
    };
    let Some(historical) = modes
        .iter()
        .find(|m| matches!(m.mode, FeedMode::Historical))
    else {
        return;
    };

    // Ordered by the dataset's own schema list rather than by the map, so the prices
    // line up with the `schemas:` row directly above them.
    let priced: Vec<String> = schemas
        .iter()
        .filter_map(|schema| {
            historical
                .unit_prices
                .get(schema)
                .map(|usd| format!("{schema} {}", crate::spend::money(*usd)))
        })
        .collect();
    if !priced.is_empty() {
        println!("prices:  {} (USD per unit, historical)", priced.join(", "));
    }
}

/// The dataset's publishers: the table that decodes the `publisher_id` field in its
/// records. The upstream listing is global, so it is filtered here - OPRA alone has
/// twenty-odd publishers, and the unfiltered dump buries every other dataset's.
async fn publishers(client: &mut HistoricalClient, dataset: &str) -> Result<()> {
    let publishers = client
        .metadata()
        .list_publishers()
        .await
        .context("listing publishers")?;

    let mut shown = 0;
    for publisher in &publishers {
        if publisher.dataset.eq_ignore_ascii_case(dataset) {
            shown += 1;
            println!(
                "{id}\t{venue}\t{description}",
                id = publisher.publisher_id,
                venue = publisher.venue,
                description = publisher.description,
            );
        }
    }

    // Filtering a global list means a typo produces the same empty output a real
    // dataset with no publishers would, and exits 0. Every real dataset has at least
    // one publisher, so nothing matching means the code is wrong.
    if shown == 0 {
        bail!("no publishers for `{dataset}`; check the code against `dbnget list datasets`");
    }
    Ok(())
}
