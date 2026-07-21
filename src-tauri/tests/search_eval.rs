//! Search-quality eval: natural-language queries against a realistic corpus
//! of meetings, scored FTS-only vs hybrid (FTS + vector, RRF-fused).
//!
//! Needs a local Ollama with the embedding model — the test SKIPs (passes,
//! with a message) when either is missing, so CI without Ollama stays green.
//! Run it for real with:
//!
//!   cargo test --test search_eval -- --nocapture

use fly_app_lib::embeddings::{doc_prompt, embed_raw, query_prompt, EMBED_MODEL};
use fly_core::{Speaker, Transcript, TranscriptSegment};
use fly_storage::{hybrid, SearchFilter, Storage};

const OLLAMA: &str = "http://localhost:11434";

/// (title, scratchpad, [(speaker, line)...])
type Corpus = &'static [(
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
)];

/// ~18 meetings a VC/ops user could plausibly have. Text is intentionally
/// paraphrase-rich so the queries below can avoid exact keyword overlap.
const MEETINGS: Corpus = &[
    (
        "Emirates jet fuel tender",
        "- Karim thinks our per-gallon number is too high\n- resubmit before Thursday",
        &[
            ("You", "Let's go through the tender submission line by line."),
            ("Karim", "Our price per gallon is above the market. If we want the airline contract we need to sharpen the number."),
            ("You", "How much room do we have before the margin goes negative?"),
            ("Karim", "About four percent. I'd bid two percent lower and keep hedging costs inside that."),
            ("You", "Okay, we resubmit the bid on Thursday with the lower price."),
        ],
    ),
    (
        "Q3 budget review",
        "",
        &[
            ("Farah", "Headcount is the biggest line. We freeze all non-engineering requisitions until October."),
            ("You", "What about travel and events?"),
            ("Farah", "Travel gets cut by a third. Events stay because the conference is already paid for."),
        ],
    ),
    (
        "Checkout outage postmortem",
        "root cause: pool exhaustion under flash sale load",
        &[
            ("Deepak", "At 9:14 the payment page started returning five hundreds. Carts were dropping for forty minutes."),
            ("You", "What was the root cause?"),
            ("Deepak", "The database connection pool was exhausted. The flash sale tripled traffic and the pool cap was still at the old size."),
            ("You", "Action items: raise the cap, add an alert on pool saturation, and load test before the next promotion."),
        ],
    ),
    (
        "Onboarding flow design review",
        "",
        &[
            ("Mina", "Half the new signups never finish the second step. The form asks for too much before showing any value."),
            ("You", "Can we defer the company profile questions?"),
            ("Mina", "Yes — collect just the email up front, everything else after the first session."),
        ],
    ),
    (
        "1:1 with Priya",
        "promotion packet due next cycle",
        &[
            ("You", "You've been leading the platform work for two quarters now."),
            ("Priya", "I'd like to make the case for the senior title this cycle."),
            ("You", "I agree. Let's assemble the packet — peer feedback, the migration project, and the incident response work."),
        ],
    ),
    (
        "Snowflake renewal negotiation",
        "",
        &[
            ("Tom", "Their first quote for the renewal was up eighteen percent year over year."),
            ("You", "What do we get if we commit for three years?"),
            ("Tom", "A twenty-five percent discount and a capacity rollover clause. I think we take the multi-year deal."),
            ("You", "Agreed — sign the three-year commit at the discounted rate."),
        ],
    ),
    (
        "Sales pipeline weekly",
        "",
        &[
            ("Ana", "Two enterprise proof-of-concepts slipped to next quarter. The champions went quiet after the security review."),
            ("You", "Push the security questionnaire earlier in the cycle so it stops stalling late-stage deals."),
        ],
    ),
    (
        "Hiring sync — backend loop",
        "",
        &[
            ("Rosa", "The panel was unanimous on Marcus for the server-side role. Strong systems depth, great debugging exercise."),
            ("You", "Comp within band?"),
            ("Rosa", "Yes. I'll send the offer letter for approval today."),
        ],
    ),
    (
        "H2 product roadmap",
        "",
        &[
            ("Leo", "For the second half the phone experience is the priority: offline mode ships in August, push notifications in September."),
            ("You", "Desktop parity waits until the app store rating recovers."),
        ],
    ),
    (
        "Launch readiness — press",
        "",
        &[
            ("Sofia", "The embargo lifts Tuesday at six in the morning Pacific. Briefings with the three outlets are done."),
            ("You", "So the public announcement is Tuesday, and the blog post goes live the same minute."),
        ],
    ),
    (
        "Churn deep-dive",
        "",
        &[
            ("Omar", "Most of the accounts that cancel do it in the first ninety days. The top stated reason is that setup took too long."),
            ("You", "So the save offer should be onboarding help, not a discount."),
            ("Omar", "Right — the discount cohort churned again within two months anyway."),
        ],
    ),
    (
        "Security audit kickoff",
        "",
        &[
            ("Wei", "The external testers start Monday. Scope is the public API, the web app, and the mobile clients."),
            ("You", "Is the compliance report part of this engagement?"),
            ("Wei", "The SOC 2 evidence collection runs in parallel, same firm."),
        ],
    ),
    (
        "Office move logistics",
        "",
        &[
            ("Nadia", "The lease on the seventh floor starts May first. Desks arrive the week before."),
            ("You", "Badge access has to work on day one, and the movers come the last weekend of April."),
        ],
    ),
    (
        "Reunión con distribuidores",
        "",
        &[
            ("Diego", "Los mayoristas piden un descuento por volumen del quince por ciento a partir de mil unidades."),
            ("You", "Podemos llegar al doce por ciento si firman el pedido anual completo."),
            ("Diego", "Se lo propongo y cerramos el precio la próxima semana."),
        ],
    ),
    (
        "Board prep",
        "",
        &[
            ("Elena", "Net burn is four hundred thousand a month. With the current balance that's nineteen months of runway."),
            ("You", "The deck should lead with the revenue ramp, then the cash position."),
        ],
    ),
    (
        "API rate limiting design",
        "",
        &[
            ("Ivan", "A handful of integrations hammer the endpoints. I propose per-key quotas with a burst allowance."),
            ("You", "Return a retry-after header and give the big customers a paid tier with a higher ceiling."),
        ],
    ),
    (
        "Support escalation review",
        "",
        &[
            ("Grace", "We missed the response-time target on eleven tickets last week, all in the European morning."),
            ("You", "Coverage gap before the US shift starts. Let's add a rotation for those hours."),
        ],
    ),
    (
        "Maersk partnership pilot",
        "",
        &[
            ("Henrik", "The pilot integrates our tracking data with their ocean freight schedules — thirty containers on the Rotterdam route."),
            ("You", "If the shipment visibility numbers hold, we expand to the full lane in the fall."),
        ],
    ),
];

/// (natural-language query, index into MEETINGS of the expected top meeting)
const QUERIES: &[(&str, usize)] = &[
    (
        "the meeting where we discussed fuel bid pricing with Karim",
        0,
    ),
    ("what did we freeze in the budget", 1),
    ("why did the payment page break during the sale", 2),
    ("meeting about new users dropping off during signup", 3),
    ("Priya's promotion discussion", 4),
    (
        "what did we decide about the data warehouse contract renewal",
        5,
    ),
    ("why are enterprise deals getting stuck", 6),
    ("which candidate got the offer for the server role", 7),
    ("mobile plans for the second half of the year", 8),
    ("when does the press embargo lift", 9),
    ("why customers cancel in the first three months", 10),
    ("penetration test scope", 11),
    ("when do we get access to the new office", 12),
    (
        "volume discount talks with the Spanish-speaking distributors",
        13,
    ),
    ("how many months of cash do we have left", 14),
    ("plan for throttling heavy API users", 15),
    // keyword queries — FTS should already nail these; hybrid must not
    // regress them
    ("snowflake renewal", 5),
    ("maersk", 17),
    ("connection pool", 2),
    ("embargo", 9),
    // hard paraphrases — no content word overlaps the corpus text
    ("the incident where we lost sales for forty minutes", 2),
    ("giving important clients higher limits", 15),
];

async fn ollama_ready() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let Ok(resp) = client.get(format!("{OLLAMA}/api/tags")).send().await else {
        return false;
    };
    let Ok(body) = resp.text().await else {
        return false;
    };
    body.contains(EMBED_MODEL)
}

fn seed(storage: &Storage) -> Vec<String> {
    let mut note_ids = Vec::new();
    for (title, scratchpad, lines) in MEETINGS {
        let note = storage.create_note(title, None).unwrap();
        if !scratchpad.is_empty() {
            storage
                .update_note_scratchpad(&note.id, scratchpad)
                .unwrap();
        }
        let meeting = storage.create_meeting(title, &note.id, &[]).unwrap();
        let mut speakers: Vec<Speaker> = Vec::new();
        let mut segments = Vec::new();
        for (i, (who, text)) in lines.iter().enumerate() {
            let key = format!("spk_{who}");
            if !speakers.iter().any(|s| s.key == key) {
                speakers.push(Speaker {
                    key: key.clone(),
                    label: (*who).to_string(),
                });
            }
            segments.push(TranscriptSegment {
                id: format!("seg-{i}"),
                speaker_key: key,
                start_ms: i as u64 * 5000,
                end_ms: i as u64 * 5000 + 4000,
                text: (*text).to_string(),
                words: vec![],
            });
        }
        storage
            .save_transcript(&Transcript {
                meeting_id: meeting.id.clone(),
                language: None,
                engine: "eval".into(),
                segments,
                speakers,
            })
            .unwrap();
        note_ids.push(note.id);
    }
    note_ids
}

async fn embed_all_pending(storage: &Storage) {
    loop {
        let batch = storage.pending_embedding_chunks(EMBED_MODEL, 32).unwrap();
        if batch.is_empty() {
            return;
        }
        let inputs: Vec<String> = batch
            .iter()
            .map(|c| doc_prompt(&c.title, &c.text))
            .collect();
        let vectors = embed_raw(OLLAMA, &inputs).await.expect("embed batch");
        let rows: Vec<(i64, Vec<f32>)> = batch.iter().map(|c| c.id).zip(vectors).collect();
        storage.store_chunk_embeddings(EMBED_MODEL, &rows).unwrap();
    }
}

/// Rank of `expected` in a fused result list (None = absent).
fn rank_of(hits: &[fly_storage::SearchHit], expected: &str) -> Option<usize> {
    hits.iter().position(|h| h.note_id == expected)
}

#[tokio::test]
async fn nl_query_hit_rate_fts_vs_hybrid() {
    if !ollama_ready().await {
        eprintln!("SKIP: Ollama with {EMBED_MODEL} not reachable at {OLLAMA}");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let note_ids = seed(&storage);
    embed_all_pending(&storage).await;
    assert_eq!(storage.embedding_backlog(EMBED_MODEL).unwrap(), 0);

    let filter = SearchFilter {
        limit: 30,
        ..Default::default()
    };
    let (mut fts1, mut fts3, mut hyb1, mut hyb3) = (0, 0, 0, 0);
    println!("\n{:<58} {:>9} {:>9}", "query", "fts", "hybrid");
    for (query, expected_idx) in QUERIES {
        let expected = &note_ids[*expected_idx];
        let (n, t) = storage.search_split(query, &filter).unwrap();
        let fts_only = hybrid::fuse(&n, &t, &[], 30);

        let qvec = embed_raw(OLLAMA, &[query_prompt(query)])
            .await
            .expect("embed query")
            .remove(0);
        let vector = storage
            .vector_search(&qvec, EMBED_MODEL, &filter, 30)
            .unwrap();
        let hybrid_hits = hybrid::fuse(&n, &t, &vector, 30);

        let fr = rank_of(&fts_only, expected);
        let hr = rank_of(&hybrid_hits, expected);
        fts1 += (fr == Some(0)) as u32;
        fts3 += fr.is_some_and(|r| r < 3) as u32;
        hyb1 += (hr == Some(0)) as u32;
        hyb3 += hr.is_some_and(|r| r < 3) as u32;
        let show = |r: Option<usize>| match r {
            Some(r) => format!("#{}", r + 1),
            None => "miss".to_string(),
        };
        println!("{:<58} {:>9} {:>9}", query, show(fr), show(hr));
    }
    let total = QUERIES.len() as u32;
    println!("\ntop-1: FTS {fts1}/{total}  hybrid {hyb1}/{total}");
    println!("top-3: FTS {fts3}/{total}  hybrid {hyb3}/{total}");

    // Regression floor, not a target: hybrid must never do worse than FTS.
    assert!(hyb1 >= fts1, "hybrid top-1 regressed below FTS-only");
    assert!(hyb3 >= fts3, "hybrid top-3 regressed below FTS-only");
}
