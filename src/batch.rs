//! `dbnget batch` - submit, poll and download Databento batch jobs.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient,
    historical::batch::{BatchJob, DownloadParams, JobState, ListJobsParams, SubmitJobParams},
};
use tracing::info;

use crate::{
    cli::{BatchCommand, BatchDownloadArgs, BatchListArgs, BatchSubmitArgs},
    query,
};

pub async fn run(client: &mut HistoricalClient, command: &BatchCommand) -> Result<()> {
    match command {
        BatchCommand::Submit(args) => submit(client, args).await,
        BatchCommand::List(args) => list(client, args).await,
        BatchCommand::Status { job_id } => status(client, job_id).await,
        BatchCommand::Download(args) => download(client, args).await,
    }
}

async fn submit(client: &mut HistoricalClient, args: &BatchSubmitArgs) -> Result<()> {
    let params = submit_params(args)?;
    let job = client
        .batch()
        .submit_job(&params)
        .await
        .context("submitting batch job")?;
    println!("{}", job.id);
    print_job(&job);

    let Some(out) = args.wait_and_download.as_deref() else {
        return Ok(());
    };
    let poll = Duration::from_secs(args.poll_interval);
    wait_for_job(client, &job.id, poll).await?;
    download_files(client, &job.id, out, None).await
}

fn submit_params(args: &BatchSubmitArgs) -> Result<SubmitJobParams> {
    Ok(SubmitJobParams::builder()
        .dataset(&args.query.dataset)
        .symbols(query::symbols(&args.query.symbols)?)
        .schema(args.query.schema)
        .date_time_range(query::date_time_range(&args.query)?)
        .stype_in(args.query.stype_in)
        .stype_out(args.query.stype_out)
        .maybe_limit(args.query.limit)
        .encoding(args.encoding.into())
        .compression(args.compression.into())
        .split_duration(args.split_duration.into())
        .split_symbols(args.split_symbols)
        .pretty_px(args.pretty_px)
        .pretty_ts(args.pretty_ts)
        .build())
}

async fn list(client: &mut HistoricalClient, args: &BatchListArgs) -> Result<()> {
    let states = if args.state.is_empty() {
        None
    } else {
        Some(args.state.iter().copied().map(JobState::from).collect())
    };
    let since = args
        .since
        .as_deref()
        .map(query::parse_instant)
        .transpose()?;
    let params = ListJobsParams::builder()
        .maybe_states(states)
        .maybe_since(since)
        .build();

    let jobs = client
        .batch()
        .list_jobs(&params)
        .await
        .context("listing batch jobs")?;
    if jobs.is_empty() {
        println!("no jobs");
    }
    for job in &jobs {
        print_job(job);
    }
    Ok(())
}

async fn status(client: &mut HistoricalClient, job_id: &str) -> Result<()> {
    let job = client
        .batch()
        .get_job_details(job_id)
        .await
        .with_context(|| format!("fetching job {job_id}"))?;
    print_job(&job);

    let files = client
        .batch()
        .list_files(job_id)
        .await
        .with_context(|| format!("listing files for job {job_id}"))?;
    for file in &files {
        println!("  {} ({} bytes)", file.filename, file.size);
    }
    Ok(())
}

async fn download(client: &mut HistoricalClient, args: &BatchDownloadArgs) -> Result<()> {
    if args.wait {
        let poll = Duration::from_secs(args.poll_interval);
        wait_for_job(client, &args.job_id, poll).await?;
    }
    download_files(client, &args.job_id, &args.out, args.filename.as_deref()).await
}

async fn download_files(
    client: &mut HistoricalClient,
    job_id: &str,
    out: &Path,
    filename: Option<&str>,
) -> Result<()> {
    tokio::fs::create_dir_all(out)
        .await
        .with_context(|| format!("creating output directory {}", out.display()))?;

    let params = DownloadParams::builder()
        .output_dir(out)
        .job_id(job_id)
        .maybe_filename_to_download(filename)
        .build();
    let paths = client
        .batch()
        .download(&params)
        .await
        .with_context(|| format!("downloading job {job_id}"))?;

    for path in &paths {
        println!("{}", path.display());
    }
    Ok(())
}

/// Polls until the job is downloadable, or fails once it can no longer become one.
async fn wait_for_job(
    client: &mut HistoricalClient,
    job_id: &str,
    poll_interval: Duration,
) -> Result<()> {
    loop {
        let job = client
            .batch()
            .get_job_details(job_id)
            .await
            .with_context(|| format!("polling job {job_id}"))?;
        match job.state {
            JobState::Done => return Ok(()),
            JobState::Expired => bail!("job {job_id} expired before it could be downloaded"),
            state => info!(
                job_id,
                ?state,
                progress = job.progress.unwrap_or(0),
                "waiting"
            ),
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn print_job(job: &BatchJob) {
    let cost = job
        .cost_usd
        .map_or_else(|| "-".to_owned(), |c| format!("${c:.2}"));
    let size = job
        .actual_size
        .map_or_else(|| "-".to_owned(), |s| s.to_string());
    println!(
        "{id}  {state:?}  {dataset} {schema} {start}..{end}  cost={cost} size={size}",
        id = job.id,
        state = job.state,
        dataset = job.dataset,
        schema = job.schema,
        start = job.start.date(),
        end = job.end.date(),
    );
}
