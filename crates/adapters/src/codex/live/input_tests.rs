use super::*;
use sylph_protocol::{CommandRequestId, RequestedNativeId};

#[tokio::test]
async fn every_input_path_echoes_its_durable_identity_even_when_transport_counters_restart() {
    let script = r#"
import sys,json
for line in sys.stdin:
    frame=json.loads(line)
    if "id" not in frame: continue
    method=frame["method"]
    if method=="model/list": result={"data":[]}
    elif method=="thread/start": result={"thread":{"id":"vendor-thread"},"model":"reported-model","reasoningEffort":"low"}
    elif method=="turn/start": result={"turn":{"id":"vendor-turn"}}
    elif method=="turn/steer": result={"turnId":"vendor-turn"}
    else: result={}
    print(json.dumps({"id":frame["id"],"result":result}),flush=True)
"#;
    let mut queue_frames = vec![];
    for incarnation in 1..=2 {
        let capture = Arc::new(process::RecordingCapture::default());
        let spawn = process::Spawn {
            program: "python3".into(),
            args: vec!["-u".into(), "-c".into(), script.into()],
            ..scripted_peer("")
        };
        let (child, lines) = Child::spawn(spawn, capture.clone()).await.unwrap();
        let session = open(
            child,
            lines,
            RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: Default::default(),
                permission_mode: None,
            },
        )
        .await
        .unwrap();
        let ids: [CommandRequestId; 3] = ["start", "steer", "queue"].map(|kind| {
            CommandRequestId::parse(format!("controller-{incarnation}-{kind}")).unwrap()
        });
        let text = "the same input bytes";
        session
            .send(
                &ids[0],
                TurnInput {
                    text: text.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let native_turn = session.last_sent_turn_id().await.unwrap();
        session
            .steer(&ids[1], text, Some(&native_turn))
            .await
            .unwrap();
        session.queue(&ids[2], text).await.unwrap();
        session.close().await.unwrap();
        let frames: Vec<Value> = capture
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|(record, _)| {
                record.transport == sylph_protocol::trace::TransportKind::LiveStdin
            })
            .filter_map(|(record, _)| serde_json::from_slice::<Value>(&record.bytes).ok())
            .filter(|frame| {
                matches!(
                    frame["method"].as_str(),
                    Some("turn/start" | "turn/steer" | "thread/queue/add")
                )
            })
            .collect();
        assert_eq!(frames.len(), 3);
        for (frame, command) in frames.iter().zip(&ids) {
            assert_eq!(
                frame["params"]["clientUserMessageId"],
                format!("sylph:input:{}", command.as_str())
            );
            assert_eq!(frame["params"]["input"][0]["text"], text);
        }
        assert_eq!(frames[1]["params"]["expectedTurnId"], native_turn);
        queue_frames.push(frames[2].clone());
    }
    assert_eq!(
        queue_frames[0]["id"], queue_frames[1]["id"],
        "the fixture must actually open the counter-reset window"
    );
    assert_ne!(
        queue_frames[0]["params"]["clientUserMessageId"],
        queue_frames[1]["params"]["clientUserMessageId"],
        "an old native queue must not mistake the new command for a repeated input"
    );
}
