use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ContestMeta {
    pub id: i64,
    pub start_time: i64,
    pub end_time: i64,
    pub penalty_minutes: i64,
    pub freeze_minutes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreboardProblem {
    pub problem_id: i64,
    pub alias: String,
    pub title: String,
    pub platform: String,
    pub external_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SubmissionForScore {
    pub id: i64,
    pub user_id: i64,
    pub display_name: String,
    pub email: String,
    pub problem_id: i64,
    pub verdict: String,
    pub submitted_at: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProblemCell {
    pub solved: bool,
    pub frozen: bool,
    pub wrong_attempts: i64,
    pub frozen_attempts: i64,
    pub solve_time_minutes: i64,
    pub penalty: i64,
    pub first_to_solve: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StandingsRow {
    pub rank: usize,
    pub user_id: i64,
    pub display_name: String,
    pub email: String,
    pub solved_count: i64,
    pub total_penalty: i64,
    pub last_ac_time: i64,
    pub problems: HashMap<i64, ProblemCell>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StandingsResponse {
    pub contest_id: i64,
    pub is_frozen: bool,
    pub problems: Vec<ScoreboardProblem>,
    pub rows: Vec<StandingsRow>,
}

pub fn compute_standings(
    contest: &ContestMeta,
    problems: Vec<ScoreboardProblem>,
    submissions: Vec<SubmissionForScore>,
    is_admin_view: bool,
    now: i64,
) -> StandingsResponse {
    let freeze_time = if contest.freeze_minutes > 0 {
        Some(contest.end_time - contest.freeze_minutes * 60)
    } else {
        None
    };
    let is_frozen_now = !is_admin_view
        && freeze_time.is_some_and(|ft| now >= ft && now < contest.end_time);

    let mut ordered = submissions;
    ordered.sort_by_key(|s| (s.submitted_at, s.id));

    let mut users: HashMap<i64, StandingsRow> = HashMap::new();
    let mut first_solve: HashMap<i64, (i64, i64, i64)> = HashMap::new(); // problem_id -> (ts, sub_id, user_id)

    for sub in ordered {
        let row = users.entry(sub.user_id).or_insert_with(|| StandingsRow {
            rank: 0,
            user_id: sub.user_id,
            display_name: sub.display_name.clone(),
            email: sub.email.clone(),
            solved_count: 0,
            total_penalty: 0,
            last_ac_time: 0,
            problems: HashMap::new(),
        });

        let cell = row.problems.entry(sub.problem_id).or_default();
        if cell.solved {
            continue;
        }
        if sub.verdict == "PENDING" || sub.verdict == "CE" {
            continue;
        }
        if is_frozen_now && freeze_time.is_some_and(|ft| sub.submitted_at >= ft) {
            cell.frozen = true;
            cell.frozen_attempts += 1;
            continue;
        }

        if sub.verdict == "AC" {
            let solve_mins = ((sub.submitted_at - contest.start_time) / 60).max(0);
            cell.solved = true;
            cell.frozen = false;
            cell.solve_time_minutes = solve_mins;
            cell.penalty = solve_mins + cell.wrong_attempts * contest.penalty_minutes;

            row.solved_count += 1;
            row.total_penalty += cell.penalty;
            row.last_ac_time = row.last_ac_time.max(sub.submitted_at);

            let replace_first = match first_solve.get(&sub.problem_id) {
                None => true,
                Some(&(prev_ts, prev_id, _)) => {
                    sub.submitted_at < prev_ts || (sub.submitted_at == prev_ts && sub.id < prev_id)
                }
            };
            if replace_first {
                first_solve.insert(sub.problem_id, (sub.submitted_at, sub.id, sub.user_id));
            }
        } else {
            cell.wrong_attempts += 1;
        }
    }

    for (pid, (_, _, uid)) in first_solve {
        if let Some(row) = users.get_mut(&uid) {
            if let Some(cell) = row.problems.get_mut(&pid) {
                cell.first_to_solve = true;
            }
        }
    }

    let mut rows: Vec<StandingsRow> = users.into_values().collect();
    rows.sort_by(|a, b| {
        b.solved_count
            .cmp(&a.solved_count)
            .then_with(|| a.total_penalty.cmp(&b.total_penalty))
            .then_with(|| a.last_ac_time.cmp(&b.last_ac_time))
    });
    for (idx, row) in rows.iter_mut().enumerate() {
        row.rank = idx + 1;
    }

    StandingsResponse {
        contest_id: contest.id,
        is_frozen: is_frozen_now,
        problems,
        rows,
    }
}
