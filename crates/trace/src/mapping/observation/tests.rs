use super::*;
use crate::mapping::{ArchiveContext,MapperState,SourceOwner};

fn fixture()->(Capture,ArchiveContext,AgentRef,SourceRef) {
    let agent=AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:root").unwrap());let source=SourceRef {issuer:Issuer::Environment(agent.environment().clone()),instance:"runtime:process/rpc".into()};let proof=ObservationRef {issuer:source.issuer.clone(),producer_id:"capture".into(),producer_epoch:"one".into(),producer_seq:1};
    let capture=Capture {observation_ref:ObservationRef {producer_seq:2,..proof.clone()},source_position:SourcePosition {source:SourceRef {instance:"runtime:process/lifecycle".into(),..source.clone()},generation:0,locator:SourceLocator::Frame {ordinal:1}},ingestion_authorization_epoch:IngestionEpoch::new("epoch").unwrap(),declared_subject_refs:vec![],captured_at:TimeEvidence {domain:"envclock".into(),value:100,unit:TimeUnit::Milliseconds,precision:None,basis:TimeBasis::SourceWall},source_clock:None,transport_kind:TransportKind::Supervisor,vendor_build:None,schema_fingerprint:None,payload_ref:PayloadRef::bytes("a".repeat(64)).unwrap(),attachments:vec![],capture_diagnostics:vec![]};
    let archive=ArchiveContext {source_owners:[((source.clone(),0),SourceOwner {agent:agent.clone(),evidence:vec![proof],rule:"native binding".into()})].into_iter().collect(),..ArchiveContext::default()};(capture,archive,agent,source)
}

#[test]
fn process_exit_closes_only_its_resource_and_never_a_child_activation() {
    let(capture,archive,_,_)=fixture();let event=SourceProcessEvent {rpc_stream:"process/rpc".into(),process_incarnation:"process".into(),event:ProcessEvent::Exited {code:Some(0),signal:None}};
    let result=ObservationMapper.map(MapInput {capture:&capture,canonical_observation:&capture.observation_ref,raw:&serde_json::to_vec(&event).unwrap(),state:&MapperState::default(),archive:&archive});
    assert!(result.claims.iter().any(|claim|matches!(&claim.fact,Fact::ResourceState {state:ResourceState::Closed {outcome:TerminalOutcome::Succeeded,..},..})));assert!(result.claims.iter().all(|claim|!matches!(&claim.fact,Fact::Execution {execution:ExecutionRef::Activation(_),..})));
    assert!(result.claims.iter().any(|claim|matches!(&claim.fact,Fact::Execution {execution:ExecutionRef::Resource(_),boundary:ExecutionBoundary::Terminated {time:Some(time),..}} if time.value==100)));
}

#[test]
fn source_pending_bytes_are_observation_coverage_not_a_failed_execution() {
    let(mut capture,archive,_,source)=fixture();capture.transport_kind=TransportKind::SourceState;let event=SourceStateEvent {source,generation:0,complete_records:3,complete_bytes:120,pending_bytes:17,following:true,state:NativeSourceState::Present};
    let result=ObservationMapper.map(MapInput {capture:&capture,canonical_observation:&capture.observation_ref,raw:&serde_json::to_vec(&event).unwrap(),state:&MapperState::default(),archive:&archive});assert_eq!(result.claims.len(),1);assert!(matches!(&result.claims[0].fact,Fact::Coverage {coverage:Coverage {state:CoverageState::Pending {..},interval:CoverageInterval::CapturedPrefix {complete_records:3,complete_bytes:120,..},..}}));
}
