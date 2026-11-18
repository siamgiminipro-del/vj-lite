use crate::db::now_unix;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct ParsedOjProblem {
    pub platform: String,
    pub external_id: String,
    pub submit_url: String,
    pub problem_url: String,
}

pub fn parse_oj_problem(platform: &str, raw: &str) -> Option<ParsedOjProblem> {
    let clean_platform = platform.trim().to_lowercase();
    let input = raw.trim();
    if input.is_empty() {
        return None;
    }

    match clean_platform.as_str() {
        "codeforces" => {
            let (contest_id, index) = if input.contains("codeforces.com") {
                let parts: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
                let mut cid = None;
                let mut idx = None;
                for w in parts.windows(2) {
                    if (w[0] == "problem" || w[0] == "contest") && w[1].chars().all(|c| c.is_ascii_digit()) {
                        cid = Some(w[1].to_string());
                    }
                }
                if let Some(last) = parts.last() {
                    if last.len() <= 3 && last.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                        idx = Some(last.to_uppercase());
                    }
                }
                (cid?, idx?)
            } else {
                let cleaned = input.replace(['/', '-', '_'], "");
                let pos = cleaned.find(|c: char| c.is_ascii_alphabetic())?;
                let (num_part, alpha_part) = cleaned.split_at(pos);
                if num_part.is_empty() || !num_part.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                (num_part.to_string(), alpha_part.to_uppercase())
            };
            let ext_id = format!("{contest_id}{index}");
            Some(ParsedOjProblem {
                platform: "codeforces".into(),
                external_id: ext_id,
                submit_url: format!("https://codeforces.com/problemset/submit/{contest_id}/{index}"),
                problem_url: format!("https://codeforces.com/problemset/problem/{contest_id}/{index}"),
            })
        }
        "atcoder" => {
            let prob_id = if input.contains("atcoder.jp") {
                input
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .last()?
                    .to_string()
            } else {
                input.to_string()
            };
            let contest_slug = prob_id.split('_').next().unwrap_or(&prob_id);
            Some(ParsedOjProblem {
                platform: "atcoder".into(),
                external_id: prob_id.clone(),
                submit_url: format!("https://atcoder.jp/contests/{contest_slug}/submit?taskScreenName={prob_id}"),
                problem_url: format!("https://atcoder.jp/contests/{contest_slug}/tasks/{prob_id}"),
            })
        }
        "cses" => {
            let task_id = input
                .split('/')
                .filter(|s| !s.is_empty())
                .last()?
                .trim()
                .to_string();
            if !task_id.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some(ParsedOjProblem {
                platform: "cses".into(),
                external_id: task_id.clone(),
                submit_url: format!("https://cses.fi/problemset/submit/{task_id}/"),
                problem_url: format!("https://cses.fi/problemset/task/{task_id}"),
            })
        }
        "codechef" => {
            let code = input
                .split('/')
                .filter(|s| !s.is_empty())
                .last()?
                .to_uppercase();
            Some(ParsedOjProblem {
                platform: "codechef".into(),
                external_id: code.clone(),
                submit_url: format!("https://www.codechef.com/submit/{code}"),
                problem_url: format!("https://www.codechef.com/problems/{code}"),
            })
        }
        _ => None,
    }
}

pub fn map_cf_verdict(v: Option<&str>) -> &'static str {
    match v {
        Some("OK") => "AC",
        Some("WRONG_ANSWER") => "WA",
        Some("TIME_LIMIT_EXCEEDED") => "TLE",
        Some("MEMORY_LIMIT_EXCEEDED") => "MLE",
        Some("COMPILATION_ERROR") => "CE",
        Some("TESTING") | None => "PENDING",
        _ => "RE",
    }
}

pub fn map_atcoder_verdict(v: &str) -> &'static str {
    match v {
        "AC" => "AC",
        "WA" => "WA",
        "TLE" => "TLE",
        "MLE" => "MLE",
        "CE" => "CE",
        "WJ" | "WR" => "PENDING",
        _ => "RE",
    }
}

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub synced_count: usize,
    pub updated_count: usize,
    pub notes: Vec<String>,
}

pub async fn sync_user_contest_ojs(
    conn: &Connection,
    http: &reqwest::Client,
    user_id: i64,
    contest_id: i64,
    cf_api_base: &str,
    atcoder_api_base: &str,
) -> Result<SyncOutcome, (u16, String)> {
    let (start_time, end_time): (i64, i64) = conn
        .query_row(
            "SELECT start_time, end_time FROM contests WHERE id = ?",
            params![contest_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| (404, "Contest not found.".to_string()))?;

    let mut handles_stmt = conn
        .prepare("SELECT platform, handle FROM oj_handles WHERE user_id = ?")
        .map_err(|e| (500, e.to_string()))?;
    let handles: Vec<(String, String)> = handles_stmt
        .query_map(params![user_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| (500, e.to_string()))?
        .filter_map(Result::ok)
        .collect();

    if handles.is_empty() {
        return Err((400, "Link at least one OJ handle (Codeforces / AtCoder) before syncing.".into()));
    }

    let mut prob_stmt = conn
        .prepare(
            "SELECT p.id, p.platform, p.external_id
             FROM contest_problems cp
             JOIN problems p ON p.id = cp.problem_id
             WHERE cp.contest_id = ? AND p.source_type = 'external'",
        )
        .map_err(|e| (500, e.to_string()))?;
    let contest_probs: Vec<(i64, String, String)> = prob_stmt
        .query_map(params![contest_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| (500, e.to_string()))?
        .filter_map(Result::ok)
        .collect();

    let mut prob_map: HashMap<(String, String), i64> = HashMap::new();
    for (pid, plat, ext_id) in contest_probs {
        prob_map.insert((plat.to_lowercase(), ext_id.to_uppercase()), pid);
    }

    let mut synced_count = 0usize;
    let mut updated_count = 0usize;
    let mut notes = Vec::new();

    for (platform, handle) in handles {
        match platform.as_str() {
            "codeforces" => {
                let url = format!("{cf_api_base}/user.status?handle={handle}&from=1&count=30");
                let resp = match http.get(&url).send().await {
                    Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
                    Ok(r) => {
                        notes.push(format!("Codeforces returned HTTP {}", r.status()));
                        None
                    }
                    Err(e) => {
                        notes.push(format!("Codeforces sync unreachable: {e}"));
                        None
                    }
                };

                if let Some(json) = resp {
                    if let Some(items) = json["result"].as_array() {
                        for item in items {
                            let created = item["creationTimeSeconds"].as_i64().unwrap_or(0);
                            if created < start_time || created > end_time {
                                continue;
                            }
                            let cid = item["problem"]["contestId"].as_i64().unwrap_or(0);
                            let idx = item["problem"]["index"].as_str().unwrap_or("").to_uppercase();
                            let key = ("codeforces".to_string(), format!("{cid}{idx}"));
                            let Some(&problem_id) = prob_map.get(&key) else {
                                continue;
                            };

                            let sub_id = item["id"].as_i64().unwrap_or(0);
                            if sub_id == 0 {
                                continue;
                            }
                            let ext_sub_id = format!("cf:{sub_id}");
                            let verdict = map_cf_verdict(item["verdict"].as_str());
                            let time_ms = item["timeConsumedMillis"].as_i64().unwrap_or(0);
                            let memory_kb = item["memoryConsumedBytes"].as_i64().unwrap_or(0) / 1024;
                            let passed = item["passedTestCount"].as_i64().unwrap_or(0);
                            let lang = item["programmingLanguage"].as_str().unwrap_or("Codeforces");

                            upsert_external_submission(
                                conn,
                                contest_id,
                                problem_id,
                                user_id,
                                "codeforces",
                                &ext_sub_id,
                                lang,
                                verdict,
                                time_ms,
                                memory_kb,
                                passed,
                                created,
                                &mut synced_count,
                                &mut updated_count,
                            );
                        }
                    }
                }
            }
            "atcoder" => {
                let url = format!("{atcoder_api_base}/atcoder-api/v3/user/submissions?user={handle}&from_second={start_time}");
                let resp = match http.get(&url).send().await {
                    Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
                    _ => None,
                };
                if let Some(items) = resp.and_then(|j| j.as_array().cloned()) {
                    for item in items {
                        let created = item["epoch_second"].as_i64().unwrap_or(0);
                        if created < start_time || created > end_time {
                            continue;
                        }
                        let prob_code = item["problem_id"].as_str().unwrap_or("").to_uppercase();
                        let key = ("atcoder".to_string(), prob_code);
                        let Some(&problem_id) = prob_map.get(&key) else {
                            continue;
                        };
                        let sub_id = item["id"].as_i64().unwrap_or(0);
                        if sub_id == 0 {
                            continue;
                        }
                        let ext_sub_id = format!("ac:{sub_id}");
                        let verdict = map_atcoder_verdict(item["result"].as_str().unwrap_or("WJ"));
                        let time_ms = item["execution_time"].as_i64().unwrap_or(0);
                        let lang = item["language"].as_str().unwrap_or("AtCoder");

                        upsert_external_submission(
                            conn,
                            contest_id,
                            problem_id,
                            user_id,
                            "atcoder",
                            &ext_sub_id,
                            lang,
                            verdict,
                            time_ms,
                            0,
                            0,
                            created,
                            &mut synced_count,
                            &mut updated_count,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let _ = conn.execute(
        "UPDATE oj_handles SET last_sync_at = ? WHERE user_id = ?",
        params![now_unix(), user_id],
    );

    Ok(SyncOutcome {
        synced_count,
        updated_count,
        notes,
    })
}

#[allow(clippy::too_many_arguments)]
fn upsert_external_submission(
    conn: &Connection,
    contest_id: i64,
    problem_id: i64,
    user_id: i64,
    platform: &str,
    ext_sub_id: &str,
    lang: &str,
    verdict: &str,
    time_ms: i64,
    memory_kb: i64,
    passed_tests: i64,
    submitted_at: i64,
    synced_count: &mut usize,
    updated_count: &mut usize,
) {
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, verdict FROM submissions WHERE external_sub_id = ?",
            params![ext_sub_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    if let Some((row_id, prev_verdict)) = existing {
        if prev_verdict == "PENDING" && verdict != "PENDING" {
            let _ = conn.execute(
                "UPDATE submissions SET verdict = ?, time_ms = ?, memory_kb = ?, passed_tests = ? WHERE id = ?",
                params![verdict, time_ms, memory_kb, passed_tests, row_id],
            );
            *updated_count += 1;
        }
    } else {
        let draft: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, code FROM external_submit_drafts
                 WHERE contest_id = ? AND problem_id = ? AND user_id = ? AND used = 0
                 ORDER BY id DESC LIMIT 1",
                params![contest_id, problem_id, user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        let code_backup = if let Some((draft_id, code)) = draft {
            let _ = conn.execute(
                "UPDATE external_submit_drafts SET used = 1 WHERE id = ?",
                params![draft_id],
            );
            code
        } else {
            String::new()
        };

        if conn
            .execute(
                "INSERT INTO submissions (
                    contest_id, problem_id, user_id, source_type, platform, external_sub_id,
                    language, code, verdict, time_ms, memory_kb, passed_tests, total_tests, error_message, submitted_at
                ) VALUES (?, ?, ?, 'external', ?, ?, ?, ?, ?, ?, ?, ?, 0, '', ?)",
                params![
                    contest_id,
                    problem_id,
                    user_id,
                    platform,
                    ext_sub_id,
                    lang,
                    code_backup,
                    verdict,
                    time_ms,
                    memory_kb,
                    passed_tests,
                    submitted_at
                ],
            )
            .is_ok()
        {
            *synced_count += 1;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchedSample {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchedProblemDetails {
    pub platform: String,
    pub external_id: String,
    pub title: String,
    pub statement: String,
    pub sample_tests: Vec<FetchedSample>,
    pub time_limit_ms: i64,
    pub memory_limit_mb: i64,
    pub problem_url: String,
    pub fetched_statement: bool,
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&le;", "<=")
        .replace("&ge;", ">=")
}

fn strip_html_tags(html: &str) -> String {
    let normalized = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n\n")
        .replace("</li>", "\n")
        .replace("</div>", "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut in_tag = false;
    for ch in normalized.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = decode_html_entities(&out);
    decoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_between<'a>(haystack: &'a str, start_pat: &str, end_pat: &str) -> Option<&'a str> {
    let s_idx = haystack.find(start_pat)? + start_pat.len();
    let rest = &haystack[s_idx..];
    let e_idx = rest.find(end_pat)?;
    Some(&rest[..e_idx])
}

fn extract_all_pre(html: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut rest = html;
    while let Some(s) = rest.find("<pre") {
        let after_open = &rest[s..];
        let Some(gt) = after_open.find('>') else { break };
        let content_start = &after_open[gt + 1..];
        let Some(end) = content_start.find("</pre>") else { break };
        let raw = &content_start[..end];
        let clean = decode_html_entities(
            &raw.replace("<br>", "\n")
                .replace("<br/>", "\n")
                .replace("<br />", "\n")
                .replace("</div>", "\n"),
        );
        let stripped = clean
            .split('<')
            .enumerate()
            .map(|(i, part)| if i == 0 { part } else { part.split_once('>').map(|x| x.1).unwrap_or("") })
            .collect::<String>();
        res.push(stripped.trim().to_string());
        rest = &content_start[end + 6..];
    }
    res
}

pub async fn fetch_oj_problem_details(
    http: &reqwest::Client,
    platform: &str,
    raw_identifier: &str,
    cf_api_base: &str,
) -> Result<FetchedProblemDetails, (u16, String)> {
    let parsed = parse_oj_problem(platform, raw_identifier)
        .ok_or((400u16, "Invalid problem ID or URL for the selected platform.".to_string()))?;

    let mut title = format!("{} {}", parsed.platform.to_uppercase(), parsed.external_id);
    let mut statement = format!(
        "Open full statement on {} ({}) and submit via Local WebView or Direct Submit.",
        parsed.platform, parsed.problem_url
    );
    let mut samples = Vec::new();
    let mut time_limit_ms = 2000i64;
    let mut memory_limit_mb = 256i64;
    let mut fetched_statement = false;

    match parsed.platform.as_str() {
        "codeforces" => {
            // 1. Always fetch exact Title from official Codeforces API
            let pos = parsed.external_id.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(0);
            let (cid_str, idx_str) = parsed.external_id.split_at(pos);
            let api_url = format!("{cf_api_base}/contest.standings?contestId={cid_str}&from=1&count=1");
            if let Ok(resp) = http.get(&api_url).send().await {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(probs) = json["result"]["problems"].as_array() {
                        if let Some(p) = probs.iter().find(|p| p["index"].as_str().unwrap_or("").eq_ignore_ascii_case(idx_str)) {
                            if let Some(name) = p["name"].as_str() {
                                title = name.to_string();
                            }
                        }
                    }
                }
            }
            // 2. Try fetching HTML statement (gracefully skips if Cloudflare blocks)
            if let Ok(resp) = http
                .get(&parsed.problem_url)
                .header("User-Agent", "Mozilla/5.0")
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(html) = resp.text().await {
                        if let Some(stmt_div) = extract_between(&html, "<div class=\"problem-statement\">", "<div class=\"sample-tests\">") {
                            let clean = strip_html_tags(stmt_div);
                            if clean.len() > 30 {
                                statement = clean.chars().take(4000).collect();
                                fetched_statement = true;
                            }
                        }
                        if let Some(sample_div) = extract_between(&html, "<div class=\"sample-tests\">", "<div class=\"note\">")
                            .or_else(|| extract_between(&html, "<div class=\"sample-tests\">", "</div></div></div>"))
                        {
                            let pres = extract_all_pre(sample_div);
                            for pair in pres.chunks(2) {
                                if pair.len() == 2 {
                                    samples.push(FetchedSample {
                                        input: format!("{}\n", pair[0]),
                                        output: format!("{}\n", pair[1]),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        "atcoder" => {
            if let Ok(resp) = http
                .get(&parsed.problem_url)
                .header("User-Agent", "Mozilla/5.0")
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(html) = resp.text().await {
                        if let Some(t) = extract_between(&html, "<span class=\"h2\">", "<a")
                            .or_else(|| extract_between(&html, "<title>", "</title>"))
                        {
                            let clean_t = strip_html_tags(t).replace('\n', " ");
                            if !clean_t.is_empty() {
                                title = clean_t;
                            }
                        }
                        if let Some(en) = extract_between(&html, "<span class=\"lang-en\">", "<span class=\"lang-ja\">")
                            .or_else(|| extract_between(&html, "<div id=\"task-statement\">", "</div>\n\t\t\t</div>"))
                        {
                            let clean = strip_html_tags(en);
                            if clean.len() > 20 {
                                statement = clean.chars().take(4000).collect();
                                fetched_statement = true;
                            }
                            let pres = extract_all_pre(en);
                            for pair in pres.chunks(2) {
                                if pair.len() == 2 {
                                    samples.push(FetchedSample {
                                        input: format!("{}\n", pair[0]),
                                        output: format!("{}\n", pair[1]),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        "cses" => {
            if let Ok(resp) = http
                .get(&parsed.problem_url)
                .header("User-Agent", "Mozilla/5.0")
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(html) = resp.text().await {
                        if let Some(t) = extract_between(&html, "<h1>", "</h1>") {
                            let clean_t = strip_html_tags(t);
                            if !clean_t.is_empty() {
                                title = clean_t;
                            }
                        }
                        if let Some(md) = extract_between(&html, "<div class=\"md\">", "</div>") {
                            let clean = strip_html_tags(md);
                            if clean.len() > 20 {
                                statement = clean.chars().take(4000).collect();
                                fetched_statement = true;
                            }
                            let pres = extract_all_pre(md);
                            for pair in pres.chunks(2) {
                                if pair.len() == 2 {
                                    samples.push(FetchedSample {
                                        input: format!("{}\n", pair[0]),
                                        output: format!("{}\n", pair[1]),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            time_limit_ms = 1000;
            memory_limit_mb = 512;
        }
        _ => {}
    }

    Ok(FetchedProblemDetails {
        platform: parsed.platform,
        external_id: parsed.external_id,
        title,
        statement,
        sample_tests: samples,
        time_limit_ms,
        memory_limit_mb,
        problem_url: parsed.problem_url,
        fetched_statement,
    })
}
