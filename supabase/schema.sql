-- VJ-Lite PostgreSQL Schema for Supabase
-- Paste this into Supabase Dashboard -> SQL Editor -> Run

CREATE TABLE IF NOT EXISTS users (
  id BIGSERIAL PRIMARY KEY,
  email TEXT UNIQUE NOT NULL,
  google_sub TEXT UNIQUE,
  display_name TEXT NOT NULL,
  password_hash TEXT DEFAULT '',
  role TEXT NOT NULL DEFAULT 'participant',
  created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  token TEXT PRIMARY KEY,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS oj_handles (
  id BIGSERIAL PRIMARY KEY,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  platform TEXT NOT NULL,
  handle TEXT NOT NULL,
  verified BOOLEAN NOT NULL DEFAULT TRUE,
  last_sync_at BIGINT NOT NULL DEFAULT 0,
  UNIQUE(user_id, platform)
);

CREATE TABLE IF NOT EXISTS problems (
  id BIGSERIAL PRIMARY KEY,
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
  created_by BIGINT REFERENCES users(id),
  created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS contests (
  id BIGSERIAL PRIMARY KEY,
  title TEXT NOT NULL,
  description TEXT DEFAULT '',
  access_code TEXT UNIQUE NOT NULL,
  start_time BIGINT NOT NULL,
  end_time BIGINT NOT NULL,
  penalty_minutes INTEGER NOT NULL DEFAULT 20,
  freeze_minutes INTEGER NOT NULL DEFAULT 0,
  created_by BIGINT REFERENCES users(id),
  created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS contest_participants (
  contest_id BIGINT NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  joined_at BIGINT NOT NULL,
  PRIMARY KEY (contest_id, user_id)
);

CREATE TABLE IF NOT EXISTS contest_problems (
  id BIGSERIAL PRIMARY KEY,
  contest_id BIGINT NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
  problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  UNIQUE(contest_id, alias)
);

CREATE TABLE IF NOT EXISTS submissions (
  id BIGSERIAL PRIMARY KEY,
  contest_id BIGINT NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
  problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
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
  submitted_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS external_submit_drafts (
  id BIGSERIAL PRIMARY KEY,
  contest_id BIGINT NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
  problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  language TEXT NOT NULL,
  code TEXT NOT NULL DEFAULT '',
  used BOOLEAN NOT NULL DEFAULT FALSE,
  created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS clarifications (
  id BIGSERIAL PRIMARY KEY,
  contest_id BIGINT NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  problem_alias TEXT DEFAULT '',
  question TEXT NOT NULL,
  answer TEXT DEFAULT '',
  is_announcement BOOLEAN NOT NULL DEFAULT FALSE,
  created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_submissions_contest ON submissions(contest_id, submitted_at, id);
