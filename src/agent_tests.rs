use super::*;
use crate::auth::Auth;
use crate::rollout::SessionIdentity;
use serde_json::json;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_waits_for_the_response_boundary_and_enters_the_next_request() {
    let mut fixture = SteeringServer::spawn();
    let (root, mut agent) = test_agent(&fixture.base_url);
    let (events_tx, mut events_rx) = unbounded_channel();
    let (handle, control) = TurnControl::channel();

    let task = tokio::spawn(async move {
        let outcome = agent
            .submit_with_control(UserInput::text("initial prompt"), events_tx, control)
            .await;
        (agent, outcome)
    });

    let first_request = fixture.requests.recv().await.expect("first request");
    let steer_id = handle
        .steer(UserInput::text("steering instruction"))
        .expect("active turn accepts steering");
    fixture
        .release_first
        .send(())
        .expect("release first response");
    let second_request = fixture.requests.recv().await.expect("second request");

    let (agent, outcome) = task.await.expect("agent task");
    assert_eq!(
        outcome.unwrap(),
        SubmitOutcome::Completed("first answer\nsecond answer".to_string())
    );
    assert_eq!(
        agent.prompt_history(),
        vec![
            "initial prompt".to_string(),
            "steering instruction".to_string()
        ]
    );
    assert!(
        handle.steer(UserInput::text("too late")).is_err(),
        "the final empty-queue check must atomically close steering admission"
    );

    assert_eq!(
        conversation_text(&first_request),
        vec![("user".to_string(), "initial prompt".to_string())]
    );
    assert_eq!(
        conversation_text(&second_request),
        vec![
            ("user".to_string(), "initial prompt".to_string()),
            ("assistant".to_string(), "first answer".to_string()),
            ("user".to_string(), "steering instruction".to_string()),
        ]
    );

    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    let response_boundary = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ModelResponseCompleted))
        .expect("first response boundary event");
    let committed = events
        .iter()
        .position(|event| *event == AgentEvent::SteeringCommitted(steer_id))
        .expect("steering commit event");
    assert!(response_boundary < committed, "{events:#?}");

    fixture.server.join().expect("server thread");
    std::fs::remove_dir_all(root).unwrap();
}

fn test_agent(base_url: &str) -> (PathBuf, Agent) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-agent-steering-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let cwd = root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    let rollout = Rollout::create_in(&root.join("state"), &cwd).unwrap();
    let identity: SessionIdentity = rollout.identity().clone();
    let conversation = Conversation::new(&cwd, rollout).unwrap();
    let mut api = ApiClient::new_with_base_url(
        Auth::for_test("token-test"),
        &identity,
        0,
        base_url.to_string(),
    )
    .unwrap();
    assert!(api.fall_back_to_http());
    let tools = ToolRuntime::new(cwd.clone(), api.web_search_client());
    (
        root,
        Agent {
            cwd,
            api,
            conversation,
            tools,
        },
    )
}

fn conversation_text(request: &[u8]) -> Vec<(String, String)> {
    let request: Value = serde_json::from_slice(request).unwrap();
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| {
            let role = item.get("role")?.as_str()?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let text = item
                .get("content")?
                .as_array()?
                .iter()
                .find_map(|content| {
                    content
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })?;
            (!text.starts_with("# Repository onboarding")).then(|| (role.to_string(), text))
        })
        .collect()
}

struct SteeringServer {
    base_url: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    release_first: std_mpsc::Sender<()>,
    server: thread::JoinHandle<()>,
}

impl SteeringServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests) = unbounded_channel();
        let (release_first, release_first_rx) = std_mpsc::channel();
        let server = thread::spawn(move || {
            for (index, answer) in ["first answer", "second answer"].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                requests_tx.send(read_request(&mut stream)).unwrap();
                if index == 0 {
                    release_first_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("first response release");
                }
                write_sse_response(&mut stream, answer);
            }
        });
        Self {
            base_url: format!("http://{address}"),
            requests,
            release_first,
            server,
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before its headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before its body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = &bytes[header_end..header_end + content_length];
    if headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("content-encoding") && value.trim() == "zstd"
        })
    }) {
        zstd::stream::decode_all(std::io::Cursor::new(body)).unwrap()
    } else {
        body.to_vec()
    }
}

fn write_sse_response(stream: &mut TcpStream, answer: &str) {
    let item = json!({
        "id": format!("msg_{}", answer.replace(' ', "_")),
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": answer}],
    });
    let item_done = json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": item,
    });
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": format!("resp_{}", answer.replace(' ', "_")),
            "model": crate::MODEL,
            "reasoning": {"context": "all_turns"},
            "output": [item],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "total_tokens": 12,
            },
        },
    });
    let body = format!("data: {item_done}\n\ndata: {completed}\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nopenai-model: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        crate::MODEL,
        body,
    )
    .unwrap();
    stream.flush().unwrap();
}
