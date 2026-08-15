//! Pricing a request, and deciding whether it is allowed to be paid for.

use std::fmt;

use anyhow::{Context, Result, bail};
use databento::{HistoricalClient, historical::metadata::GetQueryParams};

/// What the metadata endpoints say a request would cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    pub records: u64,
    pub billable_bytes: u64,
    pub usd: f64,
}

impl Quote {
    /// Whether a zero price means "already covered" or "this request matches nothing".
    ///
    /// Both quote at $0.00, and the difference matters: the first is free data, the
    /// second is a symbology or date-range mistake that would otherwise be submitted
    /// happily and deliver an empty file.
    pub fn is_empty(&self) -> bool {
        self.records == 0
    }
}

/// Renders a dollar amount at enough precision to explain itself.
///
/// Two decimals is the natural way to write money and the wrong way to write this
/// number. The cost endpoint quotes list price to sub-cent precision - a day of
/// `ohlcv-1m` for one symbol prices at $0.000479 - so a refusal formatted at two
/// decimals reads "quoted $0.00 exceeds the --spend cap of $0.00", which states that
/// zero exceeds zero and tells the user nothing about what to pass instead.
///
/// The comparison stays exact; only the rendering widens. Precision grows until a
/// significant digit appears, so ordinary prices still read as `$12.34` and only the
/// amounts that need the extra digits pay for them. An exact zero is the one amount
/// `$0.00` describes perfectly, and no amount of precision would find a digit in it.
pub fn money(usd: f64) -> String {
    if !usd.is_finite() {
        return format!("${usd}");
    }
    if usd == 0.0 {
        return "$0.00".to_owned();
    }
    if let Some(precision) = precision_for(usd) {
        let rendered = format!("{usd:.precision$}");
        return format!("${}", trimmed(&rendered));
    }
    // Smaller than the widest fixed rendering can show, but not zero: say so rather
    // than printing a row of zeroes that reads as free.
    format!("${usd:e}")
}

/// Where widening stops. Databento quotes to twelve places; past this the digits stop
/// telling a user anything they can act on, and `--spend` cannot express them anyway.
const MAX_PRECISION: usize = 8;

/// How many decimals it takes to say something about `usd`, or `None` if even the
/// widest rendering would be all zeroes.
///
/// Two decimals whenever they carry a digit. Below a cent, the first precision that
/// shows a digit and then one more: the first digit alone rounds, so $0.000479 would
/// render as `$0.0005`, a price the request does not have, in the direction that
/// overstates it. A second significant digit makes it `$0.00048`.
fn precision_for(usd: f64) -> Option<usize> {
    let significant = |s: &str| s.contains(|c: char| c.is_ascii_digit() && c != '0');
    if significant(&format!("{usd:.2}")) {
        return Some(2);
    }
    (3..=MAX_PRECISION)
        .find(|p| significant(&format!("{usd:.p$}")))
        .map(|p| (p + 1).min(MAX_PRECISION))
}

/// Drops the trailing zeroes the extra digit of precision earned, so `$0.0009` does not
/// come out as `$0.00090`. Never eats the decimal point or a whole-cent rendering.
fn trimmed(rendered: &str) -> &str {
    match rendered.split_once('.') {
        Some((_, decimals)) if decimals.len() > 2 => rendered.trim_end_matches('0'),
        _ => rendered,
    }
}

/// The smallest `--spend` value that would let `usd` through, as the user must type it.
///
/// Rounded UP at the precision it is rendered to, which is the whole point: the gate
/// compares exactly, so a suggestion rounded to nearest is refused half the time it is
/// offered, and nothing is more useless than an error message whose advice does not
/// work. The suggestion is also the MINIMUM that works - naming a round `0.01` instead
/// would ask the user to authorize twenty times the quote for no reason.
pub fn minimum_cap(usd: f64) -> String {
    let Some(precision) = precision_for(usd) else {
        // Too small to render, but not zero. There is no shorter honest suggestion than
        // the smallest amount the rendering can express.
        return format!("{:.*}", MAX_PRECISION, 1.0 / scale_for(MAX_PRECISION));
    };
    let scale = scale_for(precision);
    let ceiling = (usd * scale).ceil() / scale;
    trimmed(&format!("{ceiling:.precision$}")).to_owned()
}

/// Ten to the `precision`, built by multiplication rather than `powi` so no exponent
/// has to be cast out of the `usize` the formatter wants.
fn scale_for(precision: usize) -> f64 {
    let mut scale = 1.0;
    for _ in 0..precision {
        scale *= 10.0;
    }
    scale
}

/// Whether `spend` would let this quote through, phrased for a user deciding what to
/// pass. `--cost` exists to answer exactly this, and answering it with a number the
/// reader has to compare against a default they cannot see is only half an answer.
pub fn verdict(quote: &Quote, spend: Option<f64>) -> String {
    match approve(quote, spend) {
        Ok(()) => match spend {
            Some(cap) => format!("yes, within the --spend cap of {}", money(cap)),
            None => "yes, no --spend needed".to_owned(),
        },
        Err(err) if quote.is_empty() => format!("no - {err}"),
        Err(_) => {
            let cap = spend.map_or_else(
                || "the default $0.00 cap".to_owned(),
                |cap| format!("the --spend cap of {}", money(cap)),
            );
            format!(
                "NO - refused by {cap}; pass --spend {} to approve it",
                minimum_cap(quote.usd)
            )
        }
    }
}

impl fmt::Display for Quote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} records, {} billable bytes, {}",
            self.records,
            self.billable_bytes,
            money(self.usd)
        )?;
        if self.is_empty() {
            f.write_str(" (EMPTY - matches no records, not free)")?;
        }
        Ok(())
    }
}

/// Prices a request. Always a live call: a submission must never be gated on a cached
/// number, because the number is the thing being agreed to.
pub async fn fetch(client: &mut HistoricalClient, params: &GetQueryParams) -> Result<Quote> {
    let records = client
        .metadata()
        .get_record_count(params)
        .await
        .context("fetching record count")?;
    let billable_bytes = client
        .metadata()
        .get_billable_size(params)
        .await
        .context("fetching billable size")?;
    let usd = client
        .metadata()
        .get_cost(params)
        .await
        .context("fetching cost")?;
    Ok(Quote {
        records,
        billable_bytes,
        usd,
    })
}

/// Refuses anything `--spend` does not clearly permit.
///
/// No `--spend` means a cap of $0.00, and that is a cap on the vendor's LIST PRICE, not
/// a prediction of the bill. The cost endpoint takes no account of the account: it
/// prices a request an active subscription covers identically to one nobody has paid
/// for, and it answers in sub-cent fractions. A request whose finished job later shows
/// `cost_usd: 0.0` quotes $0.000479 before it is submitted, so the default cap refuses
/// essentially every request that holds records.
///
/// That is deliberately not papered over. Rounding the comparison to cents would make
/// the default reachable by authorizing real sub-cent charges under a cap the user set
/// to zero, and the gate is not allowed to spend money nobody approved. There is no
/// third option available: nothing the API offers before a submit distinguishes
/// "covered" from "cheap", so a `--free` mode would have nothing to compute. Passing
/// `--spend` is the user's decision to make, not ours to round away.
///
/// The comparisons are written so a non-finite quote or cap cannot pass: NaN compares
/// false against everything, so `usd <= cap` would wave one straight through.
pub fn approve(quote: &Quote, spend: Option<f64>) -> Result<()> {
    if quote.is_empty() {
        bail!("this request matches no records - check the symbols, dataset and date range");
    }
    // A negative price is not a discount, it is a response nobody designed. The gate
    // fails closed on financial values it cannot account for, and a negative quote
    // would otherwise slip under every non-negative cap, including the $0.00 default.
    if !quote.usd.is_finite() || quote.usd < 0.0 {
        bail!(
            "the quoted cost is not a sensible amount ({}) - refusing",
            quote.usd
        );
    }
    let cap = spend.unwrap_or(0.0);
    if !cap.is_finite() || cap < 0.0 {
        bail!("--spend must be a finite, non-negative number, not {cap}");
    }
    if quote.usd > cap {
        // The suggested value is the quote rounded up, not a round number, and the
        // adoption note is here because this refusal is the moment a user needs it:
        // being told a price is the moment they wonder whether they already own the
        // data, and nothing else in the output mentions that re-running adopts.
        let approve_with = minimum_cap(quote.usd);
        if spend.is_none() {
            bail!(
                "this request is priced at {} ({quote}); pass --spend {approve_with} to approve the charge.\n\
                 If a job for this exact request already exists on the account, re-running adopts it \
                 instead of buying again - `dbnget list` shows what is there.",
                money(quote.usd)
            );
        }
        bail!(
            "quoted {} exceeds the --spend cap of {}; --spend {approve_with} would approve it",
            money(quote.usd),
            money(cap)
        );
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    fn quote(usd: f64, records: u64) -> Quote {
        Quote {
            records,
            billable_bytes: 1024,
            usd,
        }
    }

    #[test]
    fn a_quote_under_the_cap_is_approved() {
        approve(&quote(5.0, 100), Some(10.0)).unwrap();
        approve(&quote(10.0, 100), Some(10.0)).unwrap();
    }

    #[test]
    fn a_quote_over_the_cap_is_refused() {
        assert!(approve(&quote(10.01, 100), Some(10.0)).is_err());
    }

    #[test]
    fn no_spend_flag_means_a_zero_cap() {
        approve(&quote(0.0, 100), None).unwrap();
        assert!(approve(&quote(0.01, 100), None).is_err());
    }

    #[test]
    fn non_finite_numbers_cannot_slip_past_the_cap() {
        assert!(approve(&quote(f64::NAN, 100), Some(10.0)).is_err());
        assert!(approve(&quote(f64::INFINITY, 100), Some(10.0)).is_err());
        assert!(approve(&quote(1.0, 100), Some(f64::NAN)).is_err());
        assert!(approve(&quote(1.0, 100), Some(f64::INFINITY)).is_err());
        // A negative quote would otherwise pass every non-negative cap, the $0.00
        // default included.
        assert!(approve(&quote(-1.0, 100), None).is_err());
        assert!(approve(&quote(-1.0, 100), Some(10.0)).is_err());
    }

    /// The refusal has to say something a user can act on. Rendered at two decimals,
    /// the real quote for a day of one symbol's minute bars reads "$0.00 exceeds the
    /// cap of $0.00", which is both self-contradictory and silent about what `--spend`
    /// would let it through.
    #[test]
    fn a_sub_cent_price_is_rendered_at_a_precision_that_shows_it() {
        assert_eq!(money(0.000_479_400_158), "$0.00048");
        assert_eq!(money(0.0009), "$0.0009");
        assert_eq!(money(0.004), "$0.004");

        let err = approve(&quote(0.000_479_400_158, 766), Some(0.0))
            .unwrap_err()
            .to_string();
        assert!(err.contains("$0.00048"), "{err}");
        assert!(!err.contains("$0.00 exceeds"), "{err}");
    }

    /// Widening is for the amounts that need it. Everything else still reads as money.
    #[test]
    fn ordinary_amounts_keep_two_decimals() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(12.5), "$12.50");
        assert_eq!(money(1234.567), "$1234.57");
        assert_eq!(money(0.01), "$0.01");
    }

    /// A suggested cap that gets refused is worse than no suggestion, so it rounds up.
    /// Rounded to nearest, the quote below would suggest `0.00047` and be refused.
    #[test]
    fn the_suggested_cap_is_the_smallest_one_that_works() {
        for usd in [0.000_479_400_158, 0.0009, 12.5, 0.004, 27.135, 1e-12] {
            let suggested: f64 = minimum_cap(usd).parse().unwrap();
            assert!(
                suggested >= usd,
                "--spend {suggested} would not admit a quote of {usd}"
            );
            approve(&quote(usd, 100), Some(suggested)).unwrap();
        }
        assert_eq!(minimum_cap(0.000_479_400_158), "0.00048");
        assert_eq!(minimum_cap(12.5), "12.50");
    }

    /// The refusal has to name a value that works, and the one a user would guess from
    /// the printed price is the one that does not.
    #[test]
    fn a_refusal_names_a_cap_that_would_be_accepted() {
        let err = approve(&quote(0.000_479_400_158, 766), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--spend 0.00048"), "{err}");
        assert!(err.contains("adopts"), "{err}");
    }

    #[test]
    fn the_verdict_says_whether_the_gate_would_pass() {
        assert!(verdict(&quote(0.5, 100), Some(1.0)).starts_with("yes"));
        assert!(verdict(&quote(0.0, 100), None).starts_with("yes"));

        let refused = verdict(&quote(0.000_479_400_158, 766), None);
        assert!(refused.contains("--spend 0.00048"), "{refused}");

        let empty = verdict(&quote(0.0, 0), Some(10.0));
        assert!(empty.starts_with("no"), "{empty}");
    }

    /// A price too small for the widest fixed rendering is still not free, and must not
    /// be printed as a row of zeroes that reads as though it were.
    #[test]
    fn an_amount_below_the_widest_rendering_does_not_print_as_zero() {
        let rendered = money(1e-12);
        assert!(!rendered.contains("0.00000000"), "{rendered}");
        assert_ne!(rendered, "$0.00");
    }

    #[test]
    fn a_free_but_empty_request_is_refused() {
        assert!(approve(&quote(0.0, 0), Some(10.0)).is_err());
        assert!(approve(&quote(0.0, 0), None).is_err());
    }
}
