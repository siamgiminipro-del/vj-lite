use rusqlite::{params, Connection, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn hash_password(password: &str) -> String {
    let salt = Uuid::new_v4().simple().to_string();
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{salt}:{digest}")
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let Some((salt, expected)) = stored.split_once(':') else {
        return false;
    };
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    let digest = hex::encode(hasher.finalize());
    digest == expected
}

pub fn open_database(db_path: &str) -> Result<Connection> {
    if db_path != ":memory:" {
        if let Some(parent) = Path::new(db_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let conn = if db_path == ":memory:" {
        Connection::open_in_memory()?
    } else {
        Connection::open(db_path)?
    };
    if db_path != ":memory:" {
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    }
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    init_schema(&conn)?;
    seed_if_empty(&conn)?;
    prune_old_contests(&conn);
    Ok(conn)
}

/// Retains at most the latest 10 contests (prioritizing active/upcoming contests, then most recent ended contests).
pub fn prune_old_contests(conn: &Connection) -> usize {
    let now = now_unix();
    conn.execute(
        "DELETE FROM contests
         WHERE id NOT IN (
             SELECT id FROM contests
             ORDER BY CASE WHEN ? <= end_time THEN 0 ELSE 1 END, id DESC
             LIMIT 10
         )",
        params![now],
    )
    .unwrap_or(0)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            email TEXT UNIQUE NOT NULL,
            google_sub TEXT UNIQUE,
            display_name TEXT NOT NULL,
            password_hash TEXT DEFAULT '',
            role TEXT NOT NULL DEFAULT 'participant',
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            expires_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS oj_handles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            platform TEXT NOT NULL,
            handle TEXT NOT NULL,
            verified INTEGER NOT NULL DEFAULT 1,
            last_sync_at INTEGER NOT NULL DEFAULT 0,
            UNIQUE(user_id, platform)
        );

        CREATE TABLE IF NOT EXISTS problems (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_type TEXT NOT NULL CHECK (source_type IN ('external', 'local')),
            platform TEXT,
            external_id TEXT,
            title TEXT NOT NULL,
            statement TEXT NOT NULL,
            input_spec TEXT DEFAULT '',
            output_spec TEXT DEFAULT '',
            sample_tests_json TEXT NOT NULL DEFAULT '[]',
            hidden_tests_json TEXT NOT NULL DEFAULT '[]',
            time_limit_ms INTEGER NOT NULL DEFAULT 2000,
            memory_limit_mb INTEGER NOT NULL DEFAULT 256,
            created_by INTEGER REFERENCES users(id),
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS contests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            description TEXT DEFAULT '',
            access_code TEXT UNIQUE NOT NULL,
            start_time INTEGER NOT NULL,
            end_time INTEGER NOT NULL,
            penalty_minutes INTEGER NOT NULL DEFAULT 20,
            freeze_minutes INTEGER NOT NULL DEFAULT 0,
            created_by INTEGER REFERENCES users(id),
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS contest_participants (
            contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            joined_at INTEGER NOT NULL,
            PRIMARY KEY (contest_id, user_id)
        );

        CREATE TABLE IF NOT EXISTS contest_problems (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
            problem_id INTEGER NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
            alias TEXT NOT NULL,
            UNIQUE(contest_id, alias)
        );

        CREATE TABLE IF NOT EXISTS submissions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
            problem_id INTEGER NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            source_type TEXT NOT NULL CHECK (source_type IN ('external', 'local')),
            platform TEXT,
            external_sub_id TEXT UNIQUE,
            language TEXT NOT NULL,
            code TEXT DEFAULT '',
            verdict TEXT NOT NULL DEFAULT 'PENDING',
            time_ms INTEGER DEFAULT 0,
            memory_kb INTEGER DEFAULT 0,
            passed_tests INTEGER DEFAULT 0,
            total_tests INTEGER DEFAULT 0,
            error_message TEXT DEFAULT '',
            submitted_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS external_submit_drafts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
            problem_id INTEGER NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            language TEXT NOT NULL,
            code TEXT NOT NULL DEFAULT '',
            used INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS clarifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            problem_alias TEXT DEFAULT '',
            question TEXT NOT NULL,
            answer TEXT DEFAULT '',
            is_announcement INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_submissions_contest ON submissions(contest_id, submitted_at, id);
        "#,
    )?;
    Ok(())
}

fn seed_if_empty(conn: &Connection) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }

    let now = now_unix();
    conn.execute(
        "INSERT INTO users (email, display_name, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)",
        params!["admin@vjclub.local", "Coordinator", hash_password("admin123"), "coordinator", now],
    )?;
    let admin_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO users (email, display_name, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)",
        params!["alice@gmail.com", "alice", hash_password("alice123"), "participant", now],
    )?;
    let alice_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO users (email, display_name, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)",
        params!["bob@gmail.com", "bob", hash_password("bob123"), "participant", now],
    )?;

    // Seed Multi-OJ Problems
    conn.execute(
        "INSERT INTO problems (source_type, platform, external_id, title, statement, input_spec, output_spec, sample_tests_json, hidden_tests_json, time_limit_ms, memory_limit_mb, created_by, created_at)
         VALUES ('external', 'codeforces', '4A', 'Watermelon', ?, ?, ?, ?, ?, 1000, 64, ?, ?)",
        params![
            "Pete and Billy want to divide a watermelon of weight w kilos so that each of the two parts weighs a positive even number of kilos.",
            "The first line contains integer w (1 <= w <= 100).",
            "Print YES if possible, otherwise NO.",
            r#"[{"input":"8\n","output":"YES\n"}]"#,
            r#"[{"input":"2\n","output":"NO\n"},{"input":"5\n","output":"NO\n"},{"input":"100\n","output":"YES\n"}]"#,
            admin_id,
            now
        ],
    )?;
    let p1 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO problems (source_type, platform, external_id, title, statement, input_spec, output_spec, sample_tests_json, hidden_tests_json, time_limit_ms, memory_limit_mb, created_by, created_at)
         VALUES ('external', 'atcoder', 'dp_a', 'Frog 1', ?, ?, ?, ?, '[]', 2000, 1024, ?, ?)",
        params![
            "There are N stones, numbered 1..N. From stone i, a frog can jump to i+1 or i+2 with cost |h_i - h_j|. Find the minimum cost to reach stone N.",
            "N followed by N stone heights h_1..h_N.",
            "Print the minimum possible total cost.",
            r#"[{"input":"4\n10 30 40 20\n","output":"30\n"}]"#,
            admin_id,
            now
        ],
    )?;
    let p2 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO problems (source_type, platform, external_id, title, statement, input_spec, output_spec, sample_tests_json, hidden_tests_json, time_limit_ms, memory_limit_mb, created_by, created_at)
         VALUES ('external', 'cses', '1094', 'Increasing Array', ?, ?, ?, ?, '[]', 1000, 512, ?, ?)",
        params![
            "You are given an array of n integers. You want to modify the array so that every element is at least as large as the previous element. On each move, you may increase the value of any element by one.",
            "First line: n. Second line: n integers.",
            "Print the minimum number of moves.",
            r#"[{"input":"5\n3 2 5 1 7\n","output":"5\n"}]"#,
            admin_id,
            now
        ],
    )?;
    let p3 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO problems (source_type, platform, external_id, title, statement, input_spec, output_spec, sample_tests_json, hidden_tests_json, time_limit_ms, memory_limit_mb, created_by, created_at)
         VALUES ('local', 'local', NULL, 'Range Sum Query', ?, ?, ?, ?, ?, 2000, 256, ?, ?)",
        params![
            "Given an integer N and an array of N integers, compute and print the total sum of all elements.",
            "First line: integer N (1 <= N <= 100000). Second line: N space-separated integers.",
            "Print a single integer: the sum of all elements.",
            r#"[{"input":"5\n1 2 3 4 5\n","output":"15\n"},{"input":"3\n-5 10 5\n","output":"10\n"}]"#,
            r#"[{"input":"1\n42\n","output":"42\n"},{"input":"4\n1000000000 1000000000 1000000000 1000000000\n","output":"4000000000\n"}]"#,
            admin_id,
            now
        ],
    )?;
    let p4 = conn.last_insert_rowid();

    // Seed Active Contest #1 with Access Code CLUB-2026-W1
    conn.execute(
        "INSERT INTO contests (title, description, access_code, start_time, end_time, penalty_minutes, freeze_minutes, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, 20, 30, ?, ?)",
        params![
            "Club Weekly Practice #1 (Multi-OJ)",
            "Mixed Codeforces, AtCoder, CSES, and Local Judge contest. Join with access code CLUB-2026-W1.",
            "CLUB-2026-W1",
            now - 1800,
            now + 7200,
            admin_id,
            now
        ],
    )?;
    let c1 = conn.last_insert_rowid();

    for (idx, pid) in [p1, p2, p3, p4].iter().enumerate() {
        let alias = ((b'A' + idx as u8) as char).to_string();
        conn.execute(
            "INSERT INTO contest_problems (contest_id, problem_id, alias) VALUES (?, ?, ?)",
            params![c1, pid, alias],
        )?;
    }

    // Seed a past contest so "Past 10 Contests Archive" is immediately visible
    conn.execute(
        "INSERT INTO contests (title, description, access_code, start_time, end_time, penalty_minutes, freeze_minutes, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, 20, 0, ?, ?)",
        params![
            "Club Kickoff Warmup (Archived)",
            "Past contest available for upsolving and standings review.",
            "WARMUP-01",
            now - 86400,
            now - 79200,
            admin_id,
            now - 86400
        ],
    )?;
    let c0 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO contest_problems (contest_id, problem_id, alias) VALUES (?, ?, 'A')",
        params![c0, p1],
    )?;
    conn.execute(
        "INSERT INTO contest_problems (contest_id, problem_id, alias) VALUES (?, ?, 'B')",
        params![c0, p4],
    )?;

    // Enroll admin and alice in Contest #1
    conn.execute(
        "INSERT INTO contest_participants (contest_id, user_id, joined_at) VALUES (?, ?, ?)",
        params![c1, admin_id, now],
    )?;
    conn.execute(
        "INSERT INTO contest_participants (contest_id, user_id, joined_at) VALUES (?, ?, ?)",
        params![c1, alice_id, now],
    )?;

    Ok(())
}
