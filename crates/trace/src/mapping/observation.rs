//! Environment acquisition state remains separate from vendor execution outcomes.
use sylph_protocol::{SourceStateEvent,NativeSourceState,SourceProcessEvent,ProcessEvent,trace::*};
use super::{ArchiveRequest,MapInput,MapOutput,MappingDiagnostic,SemanticMapper,claim};

pub struct ObservationMapper;
impl SemanticMapper for ObservationMapper {
    fn build(&self)->&'static str {"source-observation-1"}
    fn map(&self,input:MapInput<'_>)->MapOutput {
        let mut output=MapOutput {state:input.state.clone(),..MapOutput::default()};
        if input.capture.transport_kind==TransportKind::SourceState {
            let event:SourceStateEvent=match serde_json::from_slice(input.raw) {Ok(event)=>event,Err(error)=>{issue(&input,&mut output,&error.to_string());return output;}};
            let Some(owner)=input.archive.source_owners.get(&(event.source.clone(),event.generation)) else {output.requests.push(ArchiveRequest::Source {source:event.source,generation:event.generation});return output;};
            let state=match event.state {NativeSourceState::Missing=>CoverageState::Unavailable {reason:"Native source is missing; retained bytes remain readable".into()},NativeSourceState::Unreadable {reason}=>CoverageState::Unavailable {reason},NativeSourceState::Present if event.pending_bytes>0=>CoverageState::Pending {reason:format!("{} lexical bytes await a complete record",event.pending_bytes)},NativeSourceState::Present=>CoverageState::Complete};
            let mut value=claim(&input,&owner.agent,"capture/source-prefix",self.build(),Fact::Coverage {coverage:Coverage {subject:AnyObjectRef::Agent(owner.agent.clone()),source:event.source.clone(),dimension:CoverageDimension::Messages,interval:CoverageInterval::CapturedPrefix {source:event.source,generation:event.generation,complete_records:event.complete_records,complete_bytes:event.complete_bytes},state}});value.evidence.extend(owner.evidence.clone());output.claims.push(value);
        } else if input.capture.transport_kind==TransportKind::Supervisor {
            let event:SourceProcessEvent=match serde_json::from_slice(input.raw) {Ok(event)=>event,Err(error)=>{issue(&input,&mut output,&error.to_string());return output;}};
            let source=SourceRef {issuer:input.capture.observation_ref.issuer.clone(),instance:format!("runtime:{}",event.rpc_stream)};
            let Some(owner)=input.archive.source_owners.get(&(source.clone(),0)) else {output.requests.push(ArchiveRequest::Source {source,generation:0});return output;};
            let resource=ResourceRef::native(owner.agent.clone(),"sylph.vendor-process",vec![event.process_incarnation]).unwrap();
            let mut declaration=claim(&input,&owner.agent,"capture/process",self.build(),Fact::Declare {object:TraceObject::Resource(Resource {resource_ref:resource.clone(),creator:None,host:Some(OwnerRef::Agent {agent:owner.agent.clone()}),kind:ResourceKind::Extension {namespace:"sylph.capture".into(),name:"vendor_process".into()}})});declaration.evidence.extend(owner.evidence.clone());output.claims.push(declaration);
            let state=match event.event {
                ProcessEvent::Spawned {..}=>{output.claims.push(claim(&input,&owner.agent,"capture/process-start",self.build(),Fact::Execution {execution:ExecutionRef::Resource(resource.clone()),boundary:ExecutionBoundary::Started {time:Some(input.capture.captured_at.clone())}}));ResourceState::Running},
                ProcessEvent::Exited {code,signal}=>{
                    let outcome=match (code,signal) {(Some(0),None)=>TerminalOutcome::Succeeded,(Some(_),_)|(_,Some(_))=>TerminalOutcome::Failed,_=>TerminalOutcome::Unknown};let reason=Some(ExecutionCloseReason::ProcessExit {code,signal:signal.map(|signal|signal.to_string())});
                    output.claims.push(claim(&input,&owner.agent,"capture/process-exit",self.build(),Fact::Execution {execution:ExecutionRef::Resource(resource.clone()),boundary:ExecutionBoundary::Terminated {time:Some(input.capture.captured_at.clone()),reason:reason.clone()}}));
                    output.claims.push(claim(&input,&owner.agent,"capture/process-outcome",self.build(),Fact::Execution {execution:ExecutionRef::Resource(resource.clone()),boundary:ExecutionBoundary::Outcome {outcome,reason:reason.clone(),detail:None}}));
                    ResourceState::Closed {outcome,reason}
                },
                ProcessEvent::CaptureFailed {reason,..}|ProcessEvent::WaitFailed {reason}=>ResourceState::Unknown {reason},
            };
            let mut state=claim(&input,&owner.agent,"capture/process-state",self.build(),Fact::ResourceState {resource,state});state.evidence.extend(owner.evidence.clone());output.claims.push(state);
        } else {issue(&input,&mut output,"Acquisition mapper received an unsupported transport");}
        output
    }
}

fn issue(input:&MapInput<'_>,output:&mut MapOutput,message:&str) {output.diagnostics.push(MappingDiagnostic {evidence:vec![input.canonical_observation.clone()],code:DiagnosticCode::DecodeFailure,message:message.into()});}

#[cfg(test)] mod tests;
