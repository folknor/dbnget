//! `dbnget dataset` - everything about one dataset, and the dataset listing.

use anyhow::{Context, Result};
use databento::HistoricalClient;

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
    Ok(())
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
    for publisher in &publishers {
        if publisher.dataset.eq_ignore_ascii_case(dataset) {
            println!(
                "{id}\t{venue}\t{description}",
                id = publisher.publisher_id,
                venue = publisher.venue,
                description = publisher.description,
            );
        }
    }
    Ok(())
}
