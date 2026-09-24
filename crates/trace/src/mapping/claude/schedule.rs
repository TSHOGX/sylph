//! Registration, cancellation and observed listings are independent schedule facts.
use super::*;

fn reference(owner: &AgentRef, id: &str) -> ResourceRef {
    ResourceRef::native(owner.clone(), "claude.schedule", vec![id.into()]).unwrap()
}

impl Context<'_, '_> {
    pub(super) fn schedule_call(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        name: &str,
        input: &Value,
    ) {
        if name == "CronDelete" {
            if let Some(id) = text(input, "id").filter(|id| !id.is_empty()) {
                self.output.correlations.insert(
                    correlation_key(owner, "claude.schedule-target", &[operation.encode()]),
                    AnyObjectRef::Resource(reference(owner, id)),
                );
            }
        }
    }

    pub(super) fn schedule_result(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        part: &Value,
        value: &Value,
    ) {
        if part.get("is_error").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let Some(invocation) = self.input.archive.semantic.operations.get(operation) else {
            if value.get("id").is_some() || value.get("jobs").is_some() {
                self.output.requests.push(ArchiveRequest::Object {
                    object_ref: AnyObjectRef::Operation(operation.clone()),
                });
            }
            return;
        };
        let OperationCategory::Tool {
            namespace: Some(namespace),
            name,
        } = &invocation.category
        else {
            return;
        };
        if namespace != "claude"
            || !matches!(name.as_str(), "CronCreate" | "CronDelete" | "CronList")
        {
            return;
        }
        if part
            .get("is_error")
            .is_some_and(|value| !value.is_boolean())
        {
            self.issue(
                Some(owner),
                "claude/schedule-error-shape",
                DiagnosticCode::DecodeFailure,
                "Schedule result has an invalid error flag",
            );
            return;
        }
        let name = name.clone();
        match name.as_str() {
            "CronCreate" => {
                let Some(id) = text(value, "id").filter(|id| !id.is_empty()) else {
                    self.issue(
                        Some(owner),
                        "claude/schedule-create-id",
                        DiagnosticCode::DecodeFailure,
                        "Schedule creation lacks its returned ID",
                    );
                    return;
                };
                if text(value, "humanSchedule").is_none()
                    || value.get("recurring").and_then(Value::as_bool).is_none()
                    || value.get("durable").and_then(Value::as_bool).is_none()
                {
                    self.issue(
                        Some(owner),
                        "claude/schedule-create-shape",
                        DiagnosticCode::DecodeFailure,
                        "Schedule creation lacks its registration settings",
                    );
                    return;
                }
                let resource = reference(owner, id);
                self.schedule_resource(
                    owner,
                    &resource,
                    Some(operation.clone()),
                    "claude/schedule/create",
                );
                self.schedule_registration(
                    owner,
                    &resource,
                    operation,
                    value,
                    "claude/schedule/create",
                );
                self.output.correlations.insert(
                    correlation_key(owner, "claude.schedule-native-id", &[id.into()]),
                    AnyObjectRef::Resource(resource),
                );
            }
            "CronDelete" => {
                let Some(id) = text(value, "id").filter(|id| !id.is_empty()) else {
                    self.issue(
                        Some(owner),
                        "claude/schedule-delete-id",
                        DiagnosticCode::DecodeFailure,
                        "Schedule deletion lacks its returned ID",
                    );
                    return;
                };
                let requested =
                    correlation_key(owner, "claude.schedule-target", &[operation.encode()]);
                let Some(AnyObjectRef::Resource(target)) =
                    self.input.archive.correlations.get(&requested)
                else {
                    self.issue(
                        Some(owner),
                        "claude/schedule-delete-target",
                        DiagnosticCode::MissingSource,
                        "Schedule deletion has no retained requested target",
                    );
                    return;
                };
                if target != &reference(owner, id) {
                    self.issue(
                        Some(owner),
                        "claude/schedule-delete-mismatch",
                        DiagnosticCode::ConflictingIdentity,
                        "Schedule deletion returned another ID than its requested target",
                    );
                    return;
                }
                let key = correlation_key(owner, "claude.schedule-native-id", &[id.into()]);
                let Some(AnyObjectRef::Resource(resource)) =
                    self.input.archive.correlations.get(&key).cloned()
                else {
                    self.request(owner, "claude.schedule-native-id", vec![id.into()]);
                    self.issue(
                        Some(owner),
                        "claude/schedule-delete-scope",
                        DiagnosticCode::MissingSource,
                        "Successful deletion has no known schedule identity scope",
                    );
                    return;
                };
                self.add(
                    owner,
                    "claude/schedule/cancelled",
                    Fact::ResourceState {
                        resource: resource.clone(),
                        state: ResourceState::ScheduleCancelled,
                    },
                );
                self.schedule_use(owner, operation, &resource, "claude/schedule/delete");
            }
            "CronList" => {
                let Some(jobs) = value.get("jobs").and_then(Value::as_array) else {
                    self.issue(
                        Some(owner),
                        "claude/schedule-list-shape",
                        DiagnosticCode::DecodeFailure,
                        "Schedule listing has no jobs array",
                    );
                    return;
                };
                for (index, job) in jobs.iter().enumerate() {
                    let slot = format!("claude/schedule/list/{index}");
                    let (Some(id), Some(durable)) = (
                        text(job, "id").filter(|id| !id.is_empty()),
                        job.get("durable").and_then(Value::as_bool),
                    ) else {
                        self.issue(
                            Some(owner),
                            &format!("{slot}/shape"),
                            DiagnosticCode::DecodeFailure,
                            "Listed schedule lacks its ID or storage scope",
                        );
                        continue;
                    };
                    let key = correlation_key(owner, "claude.schedule-native-id", &[id.into()]);
                    let resource = match self.input.archive.correlations.get(&key) {
                        Some(AnyObjectRef::Resource(resource)) => resource.clone(),
                        _ if !durable => reference(owner, id),
                        _ => {
                            self.issue(Some(owner),&format!("{slot}/scope"),DiagnosticCode::MissingSource,"Project-wide schedule listing lacks a retained creator or cross-session identity certificate");
                            continue;
                        }
                    };
                    self.schedule_resource(owner, &resource, None, &slot);
                    self.schedule_registration(owner, &resource, operation, job, &slot);
                    self.output
                        .correlations
                        .insert(key, AnyObjectRef::Resource(resource));
                }
            }
            _ => unreachable!("closed schedule action set"),
        }
    }

    fn schedule_resource(
        &mut self,
        owner: &AgentRef,
        resource: &ResourceRef,
        creator: Option<OperationRef>,
        slot: &str,
    ) {
        self.object(
            owner,
            &format!("{slot}/resource"),
            TraceObject::Resource(Resource {
                resource_ref: resource.clone(),
                creator,
                host: None,
                kind: ResourceKind::Schedule,
            }),
        );
    }
    fn schedule_use(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        resource: &ResourceRef,
        slot: &str,
    ) {
        self.add(
            owner,
            &format!("{slot}/use"),
            Fact::Relation {
                relation: Relation::UsesResource {
                    operation: operation.clone(),
                    resource: resource.clone(),
                },
            },
        );
    }
    fn schedule_registration(
        &mut self,
        owner: &AgentRef,
        resource: &ResourceRef,
        operation: &OperationRef,
        value: &Value,
        slot: &str,
    ) {
        let registration = self.json(value, slot);
        self.add(
            owner,
            &format!("{slot}/registered"),
            Fact::ResourceState {
                resource: resource.clone(),
                state: ResourceState::ScheduleRegistered {
                    registration: registration.reference,
                },
            },
        );
        self.schedule_use(owner, operation, resource, slot);
    }
}
