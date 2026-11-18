use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::PathBuf;
use vj_server::build_app;
use vj_server::scoreboard::{compute_standings, ContestMeta, ScoreboardProblem, SubmissionForScore};

async fn spawn_test_app(
    cf_base: Option<String>,
    atcoder_base: Option<String>,
) -> (String, vj_server::routes::AppState) {
    let (router, state) = build_app(":memory:", PathBuf::from("ui"), cf_base, atcoder_base)
        .expect("Failed to build test app");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

#[tokio::test]
async fn test_health_google_auth_access_code_and_past_contests() {
    let (base, _) = spawn_test_app(None, None).await;
    let client = reqwest::Client::new();

    // 1. Health check
    let health: Value = client
        .get(format!("{base}/api/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["engine"], "rust-axum");
    assert_eq!(health["maxUsers"], 50);

    // 2. Google Sign-Up (new participant)
    let g_res = client
        .post(format!("{base}/api/auth/google"))
        .json(&json!({ "email": "newstudent@gmail.com", "displayName": "New Student" }))
        .send()
        .await
        .unwrap();
    assert_eq!(g_res.status(), 200);
    let g_json: Value = g_res.json().await.unwrap();
    let token = g_json["token"].as_str().unwrap().to_string();
    assert_eq!(g_json["user"]["email"], "newstudent@gmail.com");

    // 3. Join contest via Access Code CLUB-2026-W1
    let join_res = client
        .post(format!("{base}/api/contests/join"))
        .bearer_auth(&token)
        .json(&json!({ "accessCode": "CLUB-2026-W1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(join_res.status(), 200);
    let join_json: Value = join_res.json().await.unwrap();
    assert_eq!(join_json["ok"], true);

    // 4. Past 10 contests list returns seeded active + archived contest
    let list: Value = client
        .get(format!("{base}/api/contests?limit=10"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list["contests"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn test_multi_oj_sync_codeforces_and_atcoder() {
    // Spin up a tiny mock OJ API server
    let now = vj_server::db::now_unix();
    let mock_router = Router::new()
        .route(
            "/user.status",
            get(move || async move {
                Json(json!({
                    "status": "OK",
                    "result": [
                        {
                            "id": 9001,
                            "creationTimeSeconds": now - 100,
                            "problem": { "contestId": 4, "index": "A" },
                            "programmingLanguage": "GNU C++20 (64)",
                            "verdict": "OK",
                            "timeConsumedMillis": 30,
                            "memoryConsumedBytes": 1024000,
                            "passedTestCount": 10
                        }
                    ]
                }))
            }),
        )
        .route(
            "/atcoder-api/v3/user/submissions",
            get(move || async move {
                Json(json!([
                    {
                        "id": 7001,
                        "epoch_second": now - 80,
                        "problem_id": "dp_a",
                        "language": "C++ 20 (gcc 12.2)",
                        "result": "AC",
                        "execution_time": 18
                    }
                ]))
            }),
        );

    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_router).await.unwrap();
    });
    let mock_base = format!("http://{mock_addr}");

    let (base, _) = spawn_test_app(Some(mock_base.clone()), Some(mock_base)).await;
    let client = reqwest::Client::new();

    // Login as alice
    let login: Value = client
        .post(format!("{base}/api/auth/login"))
        .json(&json!({ "email": "alice@gmail.com", "password": "alice123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap().to_string();

    // Link Codeforces + AtCoder handles
    client
        .post(format!("{base}/api/oj/link"))
        .bearer_auth(&token)
        .json(&json!({ "platform": "codeforces", "handle": "alice_cf" }))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/oj/link"))
        .bearer_auth(&token)
        .json(&json!({ "platform": "atcoder", "handle": "alice_ac" }))
        .send()
        .await
        .unwrap();

    // Sync contest #1
    let sync_res: Value = client
        .post(format!("{base}/api/contests/1/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sync_res["syncedCount"], 2);

    // Standings should show 2 solved (CF 4A + AtCoder dp_a)
    let standings: Value = client
        .get(format!("{base}/api/contests/1/standings"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(standings["rows"][0]["solved_count"], 2);
}

#[tokio::test]
async fn test_local_judge_cpp_bits_stdcpp_and_python() {
    let (base, state) = spawn_test_app(None, None).await;
    let client = reqwest::Client::new();

    let login: Value = client
        .post(format!("{base}/api/auth/login"))
        .json(&json!({ "email": "alice@gmail.com", "password": "alice123" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap().to_string();

    let local_prob_id: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT id FROM problems WHERE source_type = 'local' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };

    let cpp_res = client
        .post(format!("{base}/api/contests/1/submit"))
        .bearer_auth(&token)
        .json(&json!({
            "problemId": local_prob_id,
            "language": "cpp",
            "code": "#include <bits/stdc++.h>\nusing namespace std;\nint main(){int n; long long x,s=0; cin>>n; while(n--){cin>>x;s+=x;} cout<<s<<\"\\n\";}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cpp_res.status(), 201);
    let cpp_json: Value = cpp_res.json().await.unwrap();
    assert_eq!(cpp_json["submission"]["verdict"], "AC");
    assert_eq!(cpp_json["submission"]["passed_tests"], 4);

    let py_res = client
        .post(format!("{base}/api/contests/1/submit"))
        .bearer_auth(&token)
        .json(&json!({
            "problemId": local_prob_id,
            "language": "python3",
            "code": "n = int(input())\nprint(sum(map(int, input().split())))"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(py_res.status(), 201);
    let py_json: Value = py_res.json().await.unwrap();
    assert_eq!(py_json["submission"]["verdict"], "AC");
}

#[test]
fn test_scoreboard_freeze_behavior() {
    let contest = ContestMeta {
        id: 1,
        start_time: 1000,
        end_time: 4600,
        penalty_minutes: 20,
        freeze_minutes: 15,
    };
    let probs = vec![ScoreboardProblem {
        problem_id: 10,
        alias: "A".into(),
        title: "Test".into(),
        platform: "local".into(),
        external_id: None,
    }];
    let subs = vec![SubmissionForScore {
        id: 1,
        user_id: 2,
        display_name: "alice".into(),
        email: "alice@gmail.com".into(),
        problem_id: 10,
        verdict: "AC".into(),
        submitted_at: 3900,
    }];

    let public_view = compute_standings(&contest, probs.clone(), subs.clone(), false, 4000);
    assert!(public_view.is_frozen);
    assert_eq!(public_view.rows[0].solved_count, 0);
    assert!(public_view.rows[0].problems[&10].frozen);

    let admin_view = compute_standings(&contest, probs, subs, true, 4000);
    assert!(!admin_view.is_frozen);
    assert_eq!(admin_view.rows[0].solved_count, 1);
}
