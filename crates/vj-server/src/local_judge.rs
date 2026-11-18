use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

const BITS_STDCPP_HEADER: &str = r#"#ifndef _GLIBCXX_BITS_STDCPP_H
#define _GLIBCXX_BITS_STDCPP_H
#include <iostream>
#include <iomanip>
#include <sstream>
#include <cmath>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <cctype>
#include <climits>
#include <cstdint>
#include <cassert>
#include <string>
#include <vector>
#include <array>
#include <deque>
#include <list>
#include <set>
#include <map>
#include <unordered_set>
#include <unordered_map>
#include <stack>
#include <queue>
#include <algorithm>
#include <numeric>
#include <functional>
#include <utility>
#include <tuple>
#include <bitset>
#include <iterator>
#include <memory>
#include <chrono>
#include <random>
#include <complex>
#include <limits>
#include <type_traits>
#endif
"#;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestCase {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JudgeOutcome {
    pub verdict: String,
    pub time_ms: i64,
    pub memory_kb: i64,
    pub passed_tests: i64,
    pub total_tests: i64,
    pub error_message: String,
}

pub fn normalize_output(text: &str) -> String {
    text.replace("\r\n", "\n")
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

async fn run_cmd_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    stdin_data: &str,
    timeout_ms: u64,
) -> Result<(i32, String, String, bool, i64), String> {
    let started = Instant::now();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn {program}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_data.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let timeout = Duration::from_millis(timeout_ms.max(200));
    let output_res = tokio::time::timeout(timeout, child.wait_with_output()).await;
    let elapsed_ms = started.elapsed().as_millis() as i64;

    match output_res {
        Ok(Ok(out)) => {
            let code = out.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            Ok((code, stdout, stderr, false, elapsed_ms))
        }
        Ok(Err(e)) => Err(format!("Process error: {e}")),
        Err(_) => Ok((1, String::new(), "Time limit exceeded".into(), true, elapsed_ms)),
    }
}

pub async fn judge_local(
    language: &str,
    code: &str,
    test_cases: &[TestCase],
    time_limit_ms: i64,
) -> JudgeOutcome {
    let total = test_cases.len() as i64;
    if total == 0 {
        return JudgeOutcome {
            verdict: "RE".into(),
            time_ms: 0,
            memory_kb: 0,
            passed_tests: 0,
            total_tests: 0,
            error_message: "No test cases configured.".into(),
        };
    }

    let work_dir = std::env::temp_dir().join(format!("vj-rust-run-{}", Uuid::new_v4().simple()));
    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return JudgeOutcome {
            verdict: "RE".into(),
            time_ms: 0,
            memory_kb: 0,
            passed_tests: 0,
            total_tests: total,
            error_message: format!("Failed to create sandbox dir: {e}"),
        };
    }

    let outcome = run_in_workdir(&work_dir, language, code, test_cases, time_limit_ms).await;
    let _ = std::fs::remove_dir_all(&work_dir);
    outcome
}

async fn run_in_workdir(
    work_dir: &Path,
    language: &str,
    code: &str,
    test_cases: &[TestCase],
    time_limit_ms: i64,
) -> JudgeOutcome {
    let total = test_cases.len() as i64;
    let (exec_program, exec_args): (String, Vec<String>) = match language {
        "python3" => {
            let file = work_dir.join("solution.py");
            let _ = std::fs::write(&file, code);
            ("python3".into(), vec![file.to_string_lossy().to_string()])
        }
        "javascript" => {
            let file = work_dir.join("solution.js");
            let _ = std::fs::write(&file, code);
            ("node".into(), vec![file.to_string_lossy().to_string()])
        }
        "cpp" => {
            let bits_dir = work_dir.join("bits");
            let _ = std::fs::create_dir_all(&bits_dir);
            let _ = std::fs::write(bits_dir.join("stdc++.h"), BITS_STDCPP_HEADER);

            let src_file = work_dir.join("solution.cpp");
            let bin_file = work_dir.join("solution");
            let _ = std::fs::write(&src_file, code);

            let primary = if cfg!(target_os = "macos") { "clang++" } else { "g++" };
            let fallback = if cfg!(target_os = "macos") { "g++" } else { "clang++" };
            let inc_flag = format!("-I{}", work_dir.to_string_lossy());
            let src_str = src_file.to_string_lossy().to_string();
            let bin_str = bin_file.to_string_lossy().to_string();

            let mut compile_res = run_cmd_with_timeout(
                primary,
                &["-O2", "-std=c++17", &inc_flag, &src_str, "-o", &bin_str],
                work_dir,
                "",
                10_000,
            )
            .await;

            if let Ok((code, _, ref err, _, _)) = compile_res {
                if code != 0 && err.contains("-mmacosx-version-min") {
                    compile_res = run_cmd_with_timeout(
                        fallback,
                        &["-O2", "-std=c++17", &inc_flag, &src_str, "-o", &bin_str],
                        work_dir,
                        "",
                        10_000,
                    )
                    .await;
                }
            }

            match compile_res {
                Ok((0, _, _, _, _)) => (bin_str, vec![]),
                Ok((_, _, err, _, _)) => {
                    return JudgeOutcome {
                        verdict: "CE".into(),
                        time_ms: 0,
                        memory_kb: 0,
                        passed_tests: 0,
                        total_tests: total,
                        error_message: err.chars().take(1500).collect(),
                    };
                }
                Err(e) => {
                    return JudgeOutcome {
                        verdict: "CE".into(),
                        time_ms: 0,
                        memory_kb: 0,
                        passed_tests: 0,
                        total_tests: total,
                        error_message: e,
                    };
                }
            }
        }
        other => {
            return JudgeOutcome {
                verdict: "CE".into(),
                time_ms: 0,
                memory_kb: 0,
                passed_tests: 0,
                total_tests: total,
                error_message: format!("Unsupported language: {other}"),
            };
        }
    };

    let mut passed = 0i64;
    let mut max_time = 0i64;
    let arg_refs: Vec<&str> = exec_args.iter().map(|s| s.as_str()).collect();

    for (idx, tc) in test_cases.iter().enumerate() {
        match run_cmd_with_timeout(
            &exec_program,
            &arg_refs,
            work_dir,
            &tc.input,
            time_limit_ms.max(500) as u64,
        )
        .await
        {
            Ok((_, _, _, true, elapsed)) => {
                max_time = max_time.max(elapsed);
                return JudgeOutcome {
                    verdict: "TLE".into(),
                    time_ms: max_time,
                    memory_kb: 0,
                    passed_tests: passed,
                    total_tests: total,
                    error_message: format!("Time limit exceeded on test #{}", idx + 1),
                };
            }
            Ok((code, stdout, stderr, false, elapsed)) => {
                max_time = max_time.max(elapsed);
                if code != 0 {
                    return JudgeOutcome {
                        verdict: "RE".into(),
                        time_ms: max_time,
                        memory_kb: 0,
                        passed_tests: passed,
                        total_tests: total,
                        error_message: if stderr.is_empty() {
                            format!("Exited with status {code} on test #{}", idx + 1)
                        } else {
                            stderr.chars().take(800).collect()
                        },
                    };
                }
                if normalize_output(&stdout) != normalize_output(&tc.output) {
                    return JudgeOutcome {
                        verdict: "WA".into(),
                        time_ms: max_time,
                        memory_kb: 0,
                        passed_tests: passed,
                        total_tests: total,
                        error_message: format!("Wrong answer on test #{}", idx + 1),
                    };
                }
                passed += 1;
            }
            Err(e) => {
                return JudgeOutcome {
                    verdict: "RE".into(),
                    time_ms: max_time,
                    memory_kb: 0,
                    passed_tests: passed,
                    total_tests: total,
                    error_message: e,
                };
            }
        }
    }

    JudgeOutcome {
        verdict: "AC".into(),
        time_ms: max_time,
        memory_kb: 0,
        passed_tests: passed,
        total_tests: total,
        error_message: String::new(),
    }
}
