//! `dbnget meta` - the read-only metadata endpoints.

use anyhow::{Context, Result};
use databento::HistoricalClient;

use crate::{Outcome, cli::MetaCommand};

pub async fn run(client: &mut HistoricalClient, command: &MetaCommand) -> Result<Outcome> {
    match command {
        MetaCommand::Datasets => datasets(client).await,
        MetaCommand::Schemas { dataset } => schemas(client, dataset).await,
        MetaCommand::Range { dataset } => range(client, dataset).await,
        MetaCommand::Publishers => publishers(client).await,
    }?;
    Ok(Outcome::Settled)
}

async fn datasets(client: &mut HistoricalClient) -> Result<()> {
    let datasets = client
        .metadata()
        .list_datasets(None)
        .await
        .context("listing datasets")?;
    for dataset in &datasets {
        println!("{dataset}");
    }
    Ok(())
}

async fn schemas(client: &mut HistoricalClient, dataset: &str) -> Result<()> {
    let schemas = client
        .metadata()
        .list_schemas(dataset)
        .await
        .with_context(|| format!("listing schemas for {dataset}"))?;
    for schema in &schemas {
        println!("{schema}");
    }
    Ok(())
}

async fn range(client: &mut HistoricalClient, dataset: &str) -> Result<()> {
    let range = client
        .metadata()
        .get_dataset_range(dataset)
        .await
        .with_context(|| format!("fetching range for {dataset}"))?;
    println!("{} .. {}", range.start, range.end);
    Ok(())
}

async fn publishers(client: &mut HistoricalClient) -> Result<()> {
    let publishers = client
        .metadata()
        .list_publishers()
        .await
        .context("listing publishers")?;
    for publisher in &publishers {
        println!(
            "{id}\t{dataset}\t{venue}\t{description}",
            id = publisher.publisher_id,
            dataset = publisher.dataset,
            venue = publisher.venue,
            description = publisher.description,
        );
    }
    Ok(())
}
