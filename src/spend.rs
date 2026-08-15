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

/// Renders a dollar amount under one rule: **it never states a price lower than the
/// real one, and what it prints is always a `--spend` value that would be accepted.**
///
/// Two decimals is the natural way to write money and the wrong way to write this
/// number. The cost endpoint quotes list price to sub-cent precision - a day of
/// `ohlcv-1m` for one symbol prices at $0.000479 - so a refusal formatted at two
/// decimals reads "quoted $0.00 exceeds the --spend cap of $0.00", which states that
/// zero exceeds zero and tells the user nothing about what to pass instead.
///
/// Fixing that below a cent while leaving `{:.2}` above it just moved the same
/// contradiction up: a true $0.0105 printed as `$0.01` and was then refused by a $0.01
/// cap, while a genuine $0.01 was accepted by it - two requests displaying the same
/// price behaving oppositely, with nothing on screen to tell them apart. Rounding to
/// NEAREST is the whole defect, because a price that rounds down reads as affordable
/// under a cap that refuses it.
///
/// So: the shortest rendering that reproduces the value exactly, and failing that, the
/// shortest that has two significant digits and rounds UP. The comparison in `approve`
/// never widens; only the rendering does.
pub fn money(usd: f64) -> String {
    if !usd.is_finite() {
        return format!("${usd}");
    }
    if usd == 0.0 {
        return "$0.00".to_owned();
    }
    match rendering(usd) {
        Some(rendered) => format!("${rendered}"),
        // Smaller than the widest fixed rendering can show, but not zero: say so rather
        // than printing a row of zeroes that reads as free.
        None => format!("${usd:e}"),
    }
}

/// Where widening stops. Databento quotes to twelve places; past this the digits stop
/// telling a user anything they can act on, and `--spend` cannot express them anyway.
const MAX_PRECISION: usize = 8;

/// The decimal string this amount is written as, or `None` if no fixed rendering within
/// `MAX_PRECISION` can carry it.
///
/// Two passes. The first looks for a precision that round-trips - `0.01`, `12.50`,
/// `27.135`, `0.0105` all reproduce themselves - and that is both the shortest honest
/// spelling and, being exact, never an understatement.
///
/// The second is for values with more decimals than the rendering allows, like the
/// twelve-place `0.000479400158`. There is no exact spelling, so it rounds UP, at the
/// first precision carrying two significant digits: `$0.00048`. One digit would be
/// `$0.0005`, which overstates by a fifth, and rounding to nearest would give
/// `$0.00047940`, which understates and so breaks the rule the whole function exists
/// for. Above a cent this pass rounds up at the cent, which for a value needing more
/// than eight decimals is floating-point noise rather than a price - and overstating is
/// the safe direction to be wrong in.
fn rendering(usd: f64) -> Option<String> {
    // `total_cmp` rather than `==` because bit-for-bit identity is exactly what is being
    // asked: does this many decimals reproduce the price, or is a digit being lost?
    let exact = (2..=MAX_PRECISION).find(|p| {
        format!("{usd:.p$}")
            .parse::<f64>()
            .is_ok_and(|round_tripped| usd.total_cmp(&round_tripped).is_eq())
    });
    if let Some(precision) = exact {
        return Some(format!("{usd:.precision$}"));
    }

    let significant_digits = |s: &str| {
        s.trim_start_matches(['0', '.'])
            .chars()
            .filter(char::is_ascii_digit)
            .count()
    };
    (2..=MAX_PRECISION).find_map(|precision| {
        let scale = scale_for(precision);
        let rounded_up = (usd * scale).ceil() / scale;
        let rendered = format!("{rounded_up:.precision$}");
        if significant_digits(&rendered) < 2 {
            return None;
        }
        // `ceil` rounds up, but the division after it rounds to NEAREST, so the result
        // can in principle land a hair below `usd` - and this function's entire contract
        // is that it never does. A rendering that understates would be printed as the
        // price and offered as the cap that admits it, and the gate would refuse its own
        // advice. Checked in release rather than asserted, because the whole point is a
        // case too rare for a fixed spread of test values to find.
        let understates = rendered.parse::<f64>().is_ok_and(|shown| shown < usd);
        debug_assert!(!understates, "{rendered} understates {usd}");
        if understates {
            return None;
        }
        Some(rendered)
    })
}

/// The smallest `--spend` value that would let `usd` through, as the user must type it.
///
/// It is the printed price with the dollar sign taken off, and that is the point: one
/// rule, so the number on screen and the number in the advice cannot disagree. Because
/// the rendering never understates, the price shown is always itself an acceptable cap.
///
/// Deriving the suggestion separately is what let a true $0.0105 print as `$0.01` and
/// suggest `--spend 0.02` - twice the cap the request needed, from a display that had
/// already lost the digit explaining why a cent was not enough.
pub fn minimum_cap(usd: f64) -> String {
    money(usd).trim_start_matches('$').to_owned()
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
        // A quote the gate cannot account for is refused before any cap is considered,
        // and no cap would admit it: `approve` rejects a negative or non-finite cap as
        // firmly as it rejects the quote. Falling through to the suggestion would print
        // `pass --spend NaN`, which is advice that cannot work - and the rule every
        // other number in this module follows is that what is printed is a value the
        // gate would accept.
        Err(err) if !quote.usd.is_finite() || quote.usd < 0.0 => format!("no - {err}"),
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
        assert_eq!(money(0.01), "$0.01");
        assert_eq!(money(73.41), "$73.41");

        // Three decimals when two would not reproduce the price. Printing `$27.14`
        // here would be understating it by half a cent.
        assert_eq!(money(27.135), "$27.135");
    }

    /// A spread of prices: real quotes seen from the API, the awkward ones around a
    /// cent, and the extremes.
    const PRICES: [f64; 12] = [
        0.000_479_400_158,
        0.0009,
        0.004,
        0.0105,
        0.01,
        0.0199,
        0.79,
        12.5,
        24.06,
        27.135,
        73.41,
        1e-12,
    ];

    /// The rule the whole module turns on: what is printed is never less than the real
    /// price, and is always itself a cap that the gate would accept. Everything the
    /// user reads - the price, the refusal, the `--cost` verdict - inherits it.
    #[test]
    fn the_printed_price_is_always_an_acceptable_cap() {
        for usd in PRICES {
            let printed = money(usd);
            let as_cap: f64 = printed.trim_start_matches('$').parse().unwrap();
            assert!(
                as_cap >= usd,
                "{printed} understates {usd}, so it reads as affordable under a cap that refuses it"
            );
            approve(&quote(usd, 100), Some(as_cap)).unwrap();
            assert_eq!(minimum_cap(usd), printed.trim_start_matches('$'));
        }
    }

    /// The reported regression: fixed below a cent, the same contradiction remained
    /// above it. A true $0.0105 printed as `$0.01` and was refused by a $0.01 cap,
    /// while a genuine $0.01 was accepted by it - and the advice jumped a whole cent to
    /// `0.02`, twice what the request needed.
    #[test]
    fn a_price_just_over_a_cent_is_not_rounded_down_to_it() {
        assert_eq!(money(0.0105), "$0.0105");
        assert_eq!(minimum_cap(0.0105), "0.0105");
        assert_ne!(money(0.0105), money(0.01));

        // The two now behave the same way under the cap each one displays.
        approve(&quote(0.0105, 100), Some(0.0105)).unwrap();
        approve(&quote(0.01, 100), Some(0.01)).unwrap();
        assert!(approve(&quote(0.0105, 100), Some(0.01)).is_err());
    }

    /// A suggested cap that gets refused is worse than no suggestion, so it rounds up.
    /// Rounded to nearest, the quote below would suggest `0.00047` and be refused.
    #[test]
    fn the_suggested_cap_is_the_smallest_one_that_works() {
        for usd in PRICES {
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

    /// The invariant has to hold for the quotes the gate refuses outright, not just the
    /// ones it prices. A negative or non-finite quote has no cap that would admit it,
    /// so the verdict must not name one.
    #[test]
    fn a_quote_the_gate_cannot_account_for_is_never_given_a_cap_to_pass() {
        for usd in [-1.0, f64::NAN, f64::INFINITY] {
            let said = verdict(&quote(usd, 100), None);
            assert!(
                !said.contains("--spend"),
                "{said} offers a cap for a quote of {usd}"
            );
            assert!(said.starts_with("no"), "{said}");
        }
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
