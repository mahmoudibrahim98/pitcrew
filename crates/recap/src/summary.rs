//! Summaries with receipts.
//!
//! Rules first turn blocks into a [`Draft`]: sentences made of clauses, each clause a short claim
//! with the receipts behind it. A [`Summarizer`] then turns the draft into prose. The rule-based
//! [`RuleSummarizer`] joins the clauses as they are; a model-backed one may reword them, but every
//! span of its output must still cite receipts from the draft, which [`verify`] checks.

use pitcrew_protocol::model::Receipt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Text whose every clause is a span with receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The prose.
    pub text: String,
    /// Clauses of `text`, in order and not overlapping. Text outside spans is only punctuation
    /// and spaces joining them.
    pub spans: Vec<Span>,
}

/// One clause of a summary and the evidence for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Byte range in the summary text, on character boundaries.
    pub range: Range<usize>,
    /// Evidence. Never empty.
    pub receipts: Vec<Receipt>,
}

impl Summary {
    /// The text of a span.
    #[must_use]
    pub fn clause(&self, span: &Span) -> &str {
        self.text.get(span.range.clone()).unwrap_or_default()
    }

    /// Every receipt cited.
    pub fn receipts(&self) -> impl Iterator<Item = &Receipt> {
        self.spans.iter().flat_map(|s| s.receipts.iter())
    }
}

/// What a draft is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    /// One line for one block.
    Line,
    /// A paragraph for one workstream's day.
    Paragraph,
}

/// A short claim and the receipts behind it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    /// The claim, e.g. "@writer moved PAP-1 to review".
    pub text: String,
    /// Evidence. Never empty.
    pub receipts: Vec<Receipt>,
}

/// Clauses that belong in one sentence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sentence {
    /// The clauses, in order.
    pub clauses: Vec<Clause>,
}

/// The claims a summary is made from, with their receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Line or paragraph.
    pub kind: DraftKind,
    /// Sentences. A line has one.
    pub sentences: Vec<Sentence>,
}

impl Draft {
    /// Every receipt in the draft.
    pub fn receipts(&self) -> impl Iterator<Item = &Receipt> {
        self.sentences
            .iter()
            .flat_map(|s| s.clauses.iter())
            .flat_map(|c| c.receipts.iter())
    }
}

/// Why a summary could not be made or was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SummaryError {
    /// The summarizer could not run, e.g. a model call failed or was over its cap.
    #[error("summarizer unavailable: {0}")]
    Unavailable(String),
    /// A span is outside the text or not on character boundaries.
    #[error("span {0} is out of range")]
    BadRange(usize),
    /// A span starts before the previous one ends.
    #[error("span {0} overlaps the span before it")]
    Overlap(usize),
    /// A span has no receipts.
    #[error("span {0} has no receipts")]
    NoReceipts(usize),
    /// A span cites a receipt the draft does not have.
    #[error("span {0} cites a receipt that is not in the draft")]
    UnknownReceipt(usize),
}

/// Turns a draft into prose. Implementations must be deterministic for the same draft, or say
/// otherwise; every clause of the output must be a span citing receipts from the draft.
pub trait Summarizer {
    /// Writes the summary.
    ///
    /// # Errors
    ///
    /// Returns [`SummaryError::Unavailable`] when the summarizer cannot run. Callers usually fall
    /// back to [`RuleSummarizer`].
    fn summarize(&self, draft: &Draft) -> Result<Summary, SummaryError>;
}

/// Checks that a summary is well formed for its draft: spans are in range, in order, not
/// overlapping, and each cites at least one receipt, all from the draft.
///
/// # Errors
///
/// Returns the first problem found, naming the span's index.
pub fn verify(summary: &Summary, draft: &Draft) -> Result<(), SummaryError> {
    let known: HashSet<&Receipt> = draft.receipts().collect();
    let mut prev_end = 0usize;
    for (i, span) in summary.spans.iter().enumerate() {
        let Range { start, end } = span.range;
        if start >= end || summary.text.get(start..end).is_none() {
            return Err(SummaryError::BadRange(i));
        }
        if start < prev_end {
            return Err(SummaryError::Overlap(i));
        }
        if span.receipts.is_empty() {
            return Err(SummaryError::NoReceipts(i));
        }
        if span.receipts.iter().any(|r| !known.contains(r)) {
            return Err(SummaryError::UnknownReceipt(i));
        }
        prev_end = end;
    }
    Ok(())
}

/// The clauses of a sentence that can be cited: with text and receipts.
fn usable(sentence: &Sentence) -> Vec<&Clause> {
    sentence
        .clauses
        .iter()
        .filter(|c| !c.receipts.is_empty() && !c.text.is_empty())
        .collect()
}

/// Appends text and spans, tracking byte offsets.
#[derive(Default)]
struct Writer {
    summary: Summary,
}

impl Writer {
    fn sep(&mut self, s: &str) {
        self.summary.text.push_str(s);
    }

    fn clause(&mut self, text: &str, receipts: &[Receipt], capitalize: bool) {
        if receipts.is_empty() || text.is_empty() {
            return;
        }
        let start = self.summary.text.len();
        let mut chars = text.chars();
        if capitalize && let Some(first) = chars.next() {
            self.summary.text.extend(first.to_uppercase());
            self.summary.text.push_str(chars.as_str());
        } else {
            self.summary.text.push_str(text);
        }
        self.summary.spans.push(Span {
            range: start..self.summary.text.len(),
            receipts: receipts.to_vec(),
        });
    }
}

/// The default summarizer: joins clauses with commas; a paragraph's sentences start with a
/// capital and end with a full stop. Deterministic and free.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuleSummarizer;

impl RuleSummarizer {
    /// Renders a draft. Clauses without receipts are left out.
    #[must_use]
    pub fn render(&self, draft: &Draft) -> Summary {
        let mut w = Writer::default();
        let mut first_sentence = true;
        for sentence in &draft.sentences {
            let clauses = usable(sentence);
            if clauses.is_empty() {
                continue;
            }
            if !first_sentence {
                w.sep(" ");
            }
            first_sentence = false;
            for (i, clause) in clauses.iter().enumerate() {
                if i > 0 {
                    w.sep(", ");
                }
                let capitalize = draft.kind == DraftKind::Paragraph && i == 0;
                w.clause(&clause.text, &clause.receipts, capitalize);
            }
            if draft.kind == DraftKind::Paragraph {
                w.sep(".");
            }
        }
        w.summary
    }
}

impl Summarizer for RuleSummarizer {
    fn summarize(&self, draft: &Draft) -> Result<Summary, SummaryError> {
        Ok(self.render(draft))
    }
}

/// A deterministic stand-in for a model-backed summarizer, for tests. It counts its calls, and
/// can be made to fail.
#[derive(Debug, Default)]
pub struct FakeSummarizer {
    calls: AtomicUsize,
    fail: bool,
}

impl FakeSummarizer {
    /// A fake that always succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fake that always fails with [`SummaryError::Unavailable`].
    #[must_use]
    pub fn failing() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail: true,
        }
    }

    /// How many times [`Summarizer::summarize`] was called.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Summarizer for FakeSummarizer {
    /// Writes `fake: ` and then the clauses joined by ` / `, with ` // ` between sentences.
    fn summarize(&self, draft: &Draft) -> Result<Summary, SummaryError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(SummaryError::Unavailable(
                "fake summarizer set to fail".into(),
            ));
        }
        let mut w = Writer::default();
        w.sep("fake: ");
        let mut first_sentence = true;
        for sentence in &draft.sentences {
            let clauses = usable(sentence);
            if clauses.is_empty() {
                continue;
            }
            if !first_sentence {
                w.sep(" // ");
            }
            first_sentence = false;
            for (i, clause) in clauses.iter().enumerate() {
                if i > 0 {
                    w.sep(" / ");
                }
                w.clause(&clause.text, &clause.receipts, false);
            }
        }
        Ok(w.summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::EventId;

    fn ev(n: u128) -> Receipt {
        Receipt::Event {
            id: EventId(ulid::Ulid::from(n)),
        }
    }

    fn clause(text: &str, receipts: Vec<Receipt>) -> Clause {
        Clause {
            text: text.into(),
            receipts,
        }
    }

    fn paragraph() -> Draft {
        Draft {
            kind: DraftKind::Paragraph,
            sentences: vec![
                Sentence {
                    clauses: vec![
                        clause("tests failed then passed", vec![ev(1)]),
                        clause("no evidence", vec![]),
                        clause("@writer moved PAP-1 to review", vec![ev(2), ev(3)]),
                    ],
                },
                Sentence::default(),
                Sentence {
                    clauses: vec![clause("évidence first", vec![ev(4)])],
                },
            ],
        }
    }

    #[test]
    fn rule_summarizer_joins_clauses_with_spans() {
        let draft = paragraph();
        let s = RuleSummarizer.render(&draft);
        assert_eq!(
            s.text,
            "Tests failed then passed, @writer moved PAP-1 to review. Évidence first."
        );
        let clauses: Vec<&str> = s.spans.iter().map(|sp| s.clause(sp)).collect();
        assert_eq!(
            clauses,
            [
                "Tests failed then passed",
                "@writer moved PAP-1 to review",
                "Évidence first"
            ]
        );
        assert_eq!(verify(&s, &draft), Ok(()));
    }

    #[test]
    fn line_has_no_full_stop() {
        let draft = Draft {
            kind: DraftKind::Line,
            sentences: vec![Sentence {
                clauses: vec![clause("a", vec![ev(1)]), clause("b", vec![ev(2)])],
            }],
        };
        assert_eq!(RuleSummarizer.render(&draft).text, "a, b");
    }

    #[test]
    fn verify_rejects_bad_spans() {
        let draft = paragraph();
        let good = RuleSummarizer.render(&draft);
        let mut s = good.clone();
        s.spans[0].receipts.clear();
        assert_eq!(verify(&s, &draft), Err(SummaryError::NoReceipts(0)));
        let mut s = good.clone();
        s.spans[1].receipts.push(ev(99));
        assert_eq!(verify(&s, &draft), Err(SummaryError::UnknownReceipt(1)));
        let mut s = good.clone();
        s.spans[1].range.start = 0;
        assert_eq!(verify(&s, &draft), Err(SummaryError::Overlap(1)));
        let mut s = good.clone();
        s.spans[2].range.end = s.text.len() + 1;
        assert_eq!(verify(&s, &draft), Err(SummaryError::BadRange(2)));
        let mut s = good;
        let e = s.text.find('É').unwrap_or(0);
        s.spans[2].range = e + 1..e + 3;
        assert_eq!(verify(&s, &draft), Err(SummaryError::BadRange(2)));
    }

    #[test]
    fn fake_counts_calls_and_can_fail() {
        let draft = paragraph();
        let fake = FakeSummarizer::new();
        let s = fake.summarize(&draft).unwrap();
        assert_eq!(
            s.text,
            "fake: tests failed then passed / @writer moved PAP-1 to review // évidence first"
        );
        assert_eq!(verify(&s, &draft), Ok(()));
        fake.summarize(&draft).unwrap();
        assert_eq!(fake.calls(), 2);
        let failing = FakeSummarizer::failing();
        assert!(matches!(
            failing.summarize(&draft),
            Err(SummaryError::Unavailable(_))
        ));
        assert_eq!(failing.calls(), 1);
    }
}
