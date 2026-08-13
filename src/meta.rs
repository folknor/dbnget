//! `dbnget meta` and `dbnget cost` - the read-only metadata endpoints.

use anyhow::{Context, Result};
use databento::HistoricalClient;

use crate::{
    cli::{MetaCommand, QueryArgs},
    query, spend,
};

pub async fn run(client: &mut HistoricalClient, command: &MetaCommand) -> Result<()> {
    match command {
        MetaCommand::Datasets => datasets(client).await,
        MetaCommand::Schemas { dataset } => schemas(client, dataset).await,
        MetaCommand::Range { dataset } => range(client, dataset).await,
        MetaCommand::Publishers => publishers(client).await,
    }
}

/// Prints the record count, billable size and price of a query without fetching it.
///
/// A quote of $0.00 is not self-explanatory. Under an active subscription a covered
/// request prices at zero because it is already paid for, but a request whose symbols
/// match nothing prices at zero too, so the record count is printed alongside.
pub async fn cost(client: &mut HistoricalClient, args: &QueryArgs) -> Result<()> {
    let params = query::metadata_params(args)?;
    let quote = spend::fetch(client, &params).await?;

    println!("records:       {}", quote.records);
    println!("billable size: {} bytes", quote.billable_bytes);
    println!("cost:          ${:.2}", quote.usd);
    if quote.is_empty() {
        println!();
        println!("This query matches no records. A $0.00 quote here means empty, not free.");
    }
    Ok(())
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
