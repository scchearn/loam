//! Transport-free collaboration envelope types.
//!
//! Keeping the wire model independent of MQTT lets every later federation
//! layer inherit one identity and validation boundary instead of re-creating
//! it around live infrastructure.

use crate::json::{self, Value};
use chrono::{DateTime, Duration, Utc};

const DEFAULT_MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_FUTURE_EXPIRY_SECONDS: i64 = 7 * 24 * 60 * 60;
pub(crate) const MAX_MQTT_TOPIC_BYTES: usize = 65_535;

#[cfg(test)]
std::thread_local! {
    static VALIDATION_PROFILE: std::cell::Cell<Option<(u64, u128)>> = const {
        std::cell::Cell::new(None)
    };
}

/// Bounds live with the caller because deployment policy, not the wire type,
/// decides how much untrusted material and future retention it will accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationConfig {
    pub max_document_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_future_expiry: Duration,
    pub extension_schemas: Vec<SchemaBinding>,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_future_expiry: Duration::seconds(DEFAULT_MAX_FUTURE_EXPIRY_SECONDS),
            extension_schemas: Vec::new(),
        }
    }
}

/// Configured extension bindings make a type-to-schema association immutable
/// without resolving a message-supplied URL or requiring a schema registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaBinding {
    message_type: String,
    dataschema: String,
}

impl SchemaBinding {
    pub fn new(message_type: &str, dataschema: &str) -> Self {
        Self {
            message_type: message_type.to_owned(),
            dataschema: dataschema.to_owned(),
        }
    }
}

/// Stable variants let callers reject or audit one rule without parsing human
/// diagnostics, which is essential once validation sits on an authority edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    InvalidUtf8,
    InvalidJson,
    DuplicateJsonKey,
    MissingSpecversion,
    WrongSpecversion,
    MissingId,
    MissingType,
    MissingDatacontenttype,
    WrongDatacontenttype,
    InvalidIntent,
    InvalidDeliveryClass,
    MissingLatestStateKey,
    MissingLatestStateRevision,
    DuplicateEnvelopeField,
    PayloadTooLarge,
    InvalidExpiry,
    Expired,
    ExpiryTooFar,
    InvalidEnvelopeShape,
    MissingRequestThread,
    InvalidRequestCorrelation,
    MissingResponseThread,
    MissingResponseCausation,
    UnknownContextField,
    InvalidSourceScheme,
    SourceInstanceMismatch,
    UnauthorizedPrincipal,
    MissingStructuredIdentity,
    PayloadDeclaredIdentity,
    PlanLocalSubject,
    DocumentTooLarge,
    MalformedTopic,
    BindingMismatch(BindingAxis),
    UnknownRecipientKind,
    MissingRepositoryId,
    InvalidGitRefsPayload,
    MissingOldOid,
    InvalidOldOid,
    MissingNewOid,
    InvalidNewOid,
    InvalidWorkStatePayload,
    InvalidWorkState,
    MissingBaseOid,
    InvalidBaseOid,
    MissingPlanOid,
    InvalidPlanOid,
    MissingStableArtifactId,
    InvalidMessagePayload,
    MissingMessageAction,
    InvalidMessageParams,
    InvalidResponseStatus,
    MissingReviewAnchor,
    InvalidReviewCommit,
    ReservedType,
    UnnamespacedType,
    MissingSchemaMajor,
    DataschemaMismatch,
}

impl Violation {
    /// A stable, content-free token naming the rule that refused the envelope.
    ///
    /// The observable half of "stable variants let callers reject or audit one
    /// rule without parsing human diagnostics": the variant is only reachable
    /// in-process, so every surface that has to *report* a refusal — the
    /// connector's IPC reply, the delivery breadcrumbs, `federation emit` — needs
    /// one spelling of it that survives leaving the process (#102). Written out
    /// arm by arm rather than derived from the `Debug` name: a variant rename is
    /// then a deliberate change to the operator-facing vocabulary instead of a
    /// silent one, and the exhaustive match makes a new variant a compile error
    /// until it is named here.
    ///
    /// Content-free is a real guarantee, not a hope: every arm is a literal and
    /// the one payload-carrying variant composes a literal axis token, so no
    /// part of a caller's envelope can reach a log or a terminal through this.
    pub fn code(&self) -> String {
        let name = match self {
            Violation::InvalidUtf8 => "invalid_utf8",
            Violation::InvalidJson => "invalid_json",
            Violation::DuplicateJsonKey => "duplicate_json_key",
            Violation::MissingSpecversion => "missing_specversion",
            Violation::WrongSpecversion => "wrong_specversion",
            Violation::MissingId => "missing_id",
            Violation::MissingType => "missing_type",
            Violation::MissingDatacontenttype => "missing_datacontenttype",
            Violation::WrongDatacontenttype => "wrong_datacontenttype",
            Violation::InvalidIntent => "invalid_intent",
            Violation::InvalidDeliveryClass => "invalid_delivery_class",
            Violation::MissingLatestStateKey => "missing_latest_state_key",
            Violation::MissingLatestStateRevision => "missing_latest_state_revision",
            Violation::DuplicateEnvelopeField => "duplicate_envelope_field",
            Violation::PayloadTooLarge => "payload_too_large",
            Violation::InvalidExpiry => "invalid_expiry",
            Violation::Expired => "expired",
            Violation::ExpiryTooFar => "expiry_too_far",
            Violation::InvalidEnvelopeShape => "invalid_envelope_shape",
            Violation::MissingRequestThread => "missing_request_thread",
            Violation::InvalidRequestCorrelation => "invalid_request_correlation",
            Violation::MissingResponseThread => "missing_response_thread",
            Violation::MissingResponseCausation => "missing_response_causation",
            Violation::UnknownContextField => "unknown_context_field",
            Violation::InvalidSourceScheme => "invalid_source_scheme",
            Violation::SourceInstanceMismatch => "source_instance_mismatch",
            Violation::UnauthorizedPrincipal => "unauthorized_principal",
            Violation::MissingStructuredIdentity => "missing_structured_identity",
            Violation::PayloadDeclaredIdentity => "payload_declared_identity",
            Violation::PlanLocalSubject => "plan_local_subject",
            Violation::DocumentTooLarge => "document_too_large",
            Violation::MalformedTopic => "malformed_topic",
            Violation::BindingMismatch(axis) => return format!("binding_mismatch:{}", axis.code()),
            Violation::UnknownRecipientKind => "unknown_recipient_kind",
            Violation::MissingRepositoryId => "missing_repository_id",
            Violation::InvalidGitRefsPayload => "invalid_git_refs_payload",
            Violation::MissingOldOid => "missing_old_oid",
            Violation::InvalidOldOid => "invalid_old_oid",
            Violation::MissingNewOid => "missing_new_oid",
            Violation::InvalidNewOid => "invalid_new_oid",
            Violation::InvalidWorkStatePayload => "invalid_work_state_payload",
            Violation::InvalidWorkState => "invalid_work_state",
            Violation::MissingBaseOid => "missing_base_oid",
            Violation::InvalidBaseOid => "invalid_base_oid",
            Violation::MissingPlanOid => "missing_plan_oid",
            Violation::InvalidPlanOid => "invalid_plan_oid",
            Violation::MissingStableArtifactId => "missing_stable_artifact_id",
            Violation::InvalidMessagePayload => "invalid_message_payload",
            Violation::MissingMessageAction => "missing_message_action",
            Violation::InvalidMessageParams => "invalid_message_params",
            Violation::InvalidResponseStatus => "invalid_response_status",
            Violation::MissingReviewAnchor => "missing_review_anchor",
            Violation::InvalidReviewCommit => "invalid_review_commit",
            Violation::ReservedType => "reserved_type",
            Violation::UnnamespacedType => "unnamespaced_type",
            Violation::MissingSchemaMajor => "missing_schema_major",
            Violation::DataschemaMismatch => "dataschema_mismatch",
        };
        name.to_owned()
    }
}

impl BindingAxis {
    /// The axis half of a `binding_mismatch:` code. A fieldless enum, so this is
    /// a literal per arm and can carry nothing from the envelope.
    pub fn code(&self) -> &'static str {
        match self {
            BindingAxis::Organization => "organization",
            BindingAxis::Project => "project",
            BindingAxis::Origin => "origin",
            BindingAxis::DeliveryClass => "delivery_class",
            BindingAxis::StateKey => "state_key",
            BindingAxis::Audience => "audience",
            BindingAxis::RecipientKind => "recipient_kind",
            BindingAxis::Recipient => "recipient",
            BindingAxis::MessageId => "message_id",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingAxis {
    Organization,
    Project,
    Origin,
    DeliveryClass,
    StateKey,
    Audience,
    RecipientKind,
    Recipient,
    MessageId,
}

/// The broker/service supplies this authenticated identity and its explicit
/// claim set; envelope fields can be compared with it but can never enlarge it.
#[derive(Debug, Clone, Copy)]
pub struct AuthenticatedPrincipal<'a> {
    authenticated_id: &'a str,
    allowed_claims: &'a [&'a str],
}

impl<'a> AuthenticatedPrincipal<'a> {
    /// Both arguments must already use the same canonical principal namespace
    /// as `data.from.principal_id`; broker client IDs or certificate names must
    /// be mapped by the authenticating adapter before constructing this value.
    pub fn new(authenticated_id: &'a str, allowed_claims: &'a [&'a str]) -> Self {
        Self {
            authenticated_id,
            allowed_claims,
        }
    }

    fn can_claim(self, claimed: &str) -> bool {
        claimed == self.authenticated_id || self.allowed_claims.contains(&claimed)
    }
}

/// A distinct successful type prevents later transport code from accidentally
/// treating a merely parsed envelope as safe collaboration input.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedEnvelope(Envelope);

impl ValidatedEnvelope {
    pub fn as_envelope(&self) -> &Envelope {
        &self.0
    }

    pub(crate) fn into_envelope(self) -> Envelope {
        self.0
    }

    /// Extensions expose only display-safe envelope data and their opaque
    /// payload; there is deliberately no callable behavior on this view.
    pub fn extension_view(&self) -> Option<ExtensionView<'_>> {
        if is_core_type(&self.0.message_type) {
            return None;
        }
        Some(ExtensionView {
            source: &self.0.source,
            message_type: &self.0.message_type,
            summary: &self.0.data.summary,
            to: &self.0.data.to,
            context: &self.0.data.context,
            payload: &self.0.data.payload,
        })
    }
}

/// This borrowed projection is intentionally display-only: opaque extension
/// payloads remain inspectable without acquiring a dispatch capability.
pub struct ExtensionView<'a> {
    pub source: &'a str,
    pub message_type: &'a str,
    pub summary: &'a str,
    pub to: &'a [Recipient],
    pub context: &'a Context,
    pub payload: &'a Value,
}

const UNTRUSTED_BEGIN: &str = "⟦LOAM_UNTRUSTED_TEXT_BEGIN⟧";
const UNTRUSTED_END: &str = "⟦LOAM_UNTRUSTED_TEXT_END⟧";

/// Display-only wrapper for collaboration text that has crossed a trust
/// boundary. It intentionally implements no string conversion or `Display`;
/// callers must choose an output sink explicitly instead of concatenating it
/// into an instruction-bearing prompt by accident.
pub struct RenderedMessage {
    text: String,
}

impl RenderedMessage {
    pub fn write_display<W: std::fmt::Write>(&self, output: &mut W) -> std::fmt::Result {
        output.write_str(&self.text)
    }
}

/// Quotes untrusted summary/body text and disables Markdown syntax, including
/// links and fences. Rendering performs no resolution, I/O, or execution.
pub fn render_untrusted_text(summary: &str, body: Option<&str>) -> RenderedMessage {
    let mut text = String::with_capacity(
        summary.len() + body.map_or(0, str::len) + UNTRUSTED_BEGIN.len() + UNTRUSTED_END.len() + 32,
    );
    text.push_str("> ");
    text.push_str(UNTRUSTED_BEGIN);
    text.push('\n');
    quote_sanitized(&mut text, "summary", summary);
    if let Some(body) = body {
        text.push('\n');
        quote_sanitized(&mut text, "body", body);
    }
    text.push('\n');
    text.push_str("> ");
    text.push_str(UNTRUSTED_END);
    RenderedMessage { text }
}

fn quote_sanitized(output: &mut String, label: &str, input: &str) {
    output.push_str("> ");
    output.push_str(label);
    output.push_str(": ");
    for character in input.chars() {
        match character {
            '\n' => output.push_str("\n> "),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '⟦' => output.push_str("\\u{27e6}"),
            '⟧' => output.push_str("\\u{27e7}"),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '+'
            | '-' | '.' | '!' | '|' => {
                output.push('\\');
                output.push(character);
            }
            character if character.is_control() => output.extend(character.escape_default()),
            character => output.push(character),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Inform,
    Request,
    Response,
    Ack,
}

impl Intent {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "inform" => Some(Self::Inform),
            "request" => Some(Self::Request),
            "response" => Some(Self::Response),
            "ack" => Some(Self::Ack),
            _ => None,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Self::Inform => "inform",
            Self::Request => "request",
            Self::Response => "response",
            Self::Ack => "ack",
        }
    }
}

/// The complete wire message is owned after validation so transport code can
/// queue or retain it without borrowing the inbound frame that carried it.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub specversion: String,
    pub id: String,
    pub source: String,
    pub message_type: String,
    pub time: String,
    pub datacontenttype: String,
    pub dataschema: String,
    pub subject: Option<String>,
    pub data: Data,
    extra: Vec<(String, Value)>,
}

/// Loam-specific fields stay under `data` so broker routing metadata never
/// becomes an accidental second authority-bearing representation.
#[derive(Debug, Clone, PartialEq)]
pub struct Data {
    pub intent: Intent,
    pub from: Producer,
    pub to: Vec<Recipient>,
    pub delivery: Delivery,
    pub thread: Option<Thread>,
    pub context: Context,
    pub expires_at: String,
    pub summary: String,
    pub body: Option<Value>,
    pub payload: Value,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Producer {
    pub principal_id: String,
    pub agent_id: String,
    pub instance_id: String,
    /// The sender's given name, taken from their authenticated certificate.
    /// Provenance, never authority: it binds nothing and is rendered through
    /// the untrusted-text sanitizer like every other sender-derived string.
    pub display_name: Option<String>,
    pub runtime: Option<String>,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recipient {
    pub kind: String,
    pub id: String,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub class: String,
    pub key: Option<String>,
    pub revision: Option<String>,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub id: String,
    pub correlation_id: String,
    pub causation_id: Option<Value>,
    pub reply_to: Option<Value>,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub org_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub git: Option<GitContext>,
    pub artifacts: Vec<Artifact>,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitContext {
    pub base_oid: Option<String>,
    pub plan_oid: Option<String>,
    pub reference: Option<String>,
    pub commit: Option<String>,
    extra: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub kind: String,
    pub id: String,
    extra: Vec<(String, Value)>,
}

impl Envelope {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let specversion = take_string(&mut fields, "specversion")?;
        let id = take_string(&mut fields, "id")?;
        let source = take_string(&mut fields, "source")?;
        let message_type = take_string(&mut fields, "type")?;
        let time = take_string(&mut fields, "time")?;
        let datacontenttype = take_string(&mut fields, "datacontenttype")?;
        let dataschema = take_string(&mut fields, "dataschema")?;
        let subject = take_optional_string(&mut fields, "subject")?;
        let data = Data::from_value(take(&mut fields, "data")?)?;
        Some(Self {
            specversion,
            id,
            source,
            message_type,
            time,
            datacontenttype,
            dataschema,
            subject,
            data,
            extra: fields,
        })
    }

    /// Reconstructs the structured JSON value rather than a display rendering;
    /// downstream transport must publish the complete validated envelope.
    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(10 + self.extra.len());
        push_string(&mut fields, "specversion", self.specversion);
        push_string(&mut fields, "id", self.id);
        push_string(&mut fields, "source", self.source);
        push_string(&mut fields, "type", self.message_type);
        push_string(&mut fields, "time", self.time);
        push_string(&mut fields, "datacontenttype", self.datacontenttype);
        push_string(&mut fields, "dataschema", self.dataschema);
        if let Some(subject) = self.subject {
            push_string(&mut fields, "subject", subject);
        }
        fields.push(("data".to_owned(), self.data.into_value()));
        fields.extend(self.extra);
        Value::Object(fields)
    }

    /// Uses Loam's dependency-free JSON writer so the envelope layer does not
    /// introduce a second parser or change the runtime's portability boundary.
    pub(crate) fn to_json(&self) -> String {
        self.clone().into_value().to_json()
    }
}

impl Data {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let intent = Intent::from_wire(&take_string(&mut fields, "intent")?)?;
        let from = Producer::from_value(take(&mut fields, "from")?)?;
        let to = into_array(take(&mut fields, "to")?)?
            .into_iter()
            .map(Recipient::from_value)
            .collect::<Option<Vec<_>>>()?;
        let delivery = Delivery::from_value(take(&mut fields, "delivery")?)?;
        let thread = match take(&mut fields, "thread") {
            Some(Value::Null) | None => None,
            Some(value) => Some(Thread::from_value(value)?),
        };
        let context = Context::from_value(take(&mut fields, "context")?)?;
        let expires_at = take_string(&mut fields, "expires_at")?;
        let summary = take_string(&mut fields, "summary")?;
        let body = take(&mut fields, "body");
        let payload = take(&mut fields, "payload")?;
        Some(Self {
            intent,
            from,
            to,
            delivery,
            thread,
            context,
            expires_at,
            summary,
            body,
            payload,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(10 + self.extra.len());
        push_string(&mut fields, "intent", self.intent.as_wire().to_owned());
        fields.push(("from".to_owned(), self.from.into_value()));
        fields.push((
            "to".to_owned(),
            Value::Array(Recipient::into_values(self.to)),
        ));
        fields.push(("delivery".to_owned(), self.delivery.into_value()));
        if let Some(thread) = self.thread {
            fields.push(("thread".to_owned(), thread.into_value()));
        }
        fields.push(("context".to_owned(), self.context.into_value()));
        push_string(&mut fields, "expires_at", self.expires_at);
        push_string(&mut fields, "summary", self.summary);
        if let Some(body) = self.body {
            fields.push(("body".to_owned(), body));
        }
        fields.push(("payload".to_owned(), self.payload));
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Producer {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let principal_id = take_string(&mut fields, "principal_id")?;
        let agent_id = take_string(&mut fields, "agent_id")?;
        let instance_id = take_string(&mut fields, "instance_id")?;
        let display_name = take_optional_string(&mut fields, "display_name")?;
        let runtime = take_optional_string(&mut fields, "runtime")?;
        Some(Self {
            principal_id,
            agent_id,
            instance_id,
            display_name,
            runtime,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(4 + self.extra.len());
        push_string(&mut fields, "principal_id", self.principal_id);
        push_string(&mut fields, "agent_id", self.agent_id);
        push_string(&mut fields, "instance_id", self.instance_id);
        if let Some(display_name) = self.display_name {
            push_string(&mut fields, "display_name", display_name);
        }
        if let Some(runtime) = self.runtime {
            push_string(&mut fields, "runtime", runtime);
        }
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Recipient {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let kind = take_string(&mut fields, "kind")?;
        let id = take_string(&mut fields, "id")?;
        Some(Self {
            kind,
            id,
            extra: fields,
        })
    }

    fn into_values(recipients: Vec<Self>) -> Vec<Value> {
        recipients.into_iter().map(Self::into_value).collect()
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(2 + self.extra.len());
        push_string(&mut fields, "kind", self.kind);
        push_string(&mut fields, "id", self.id);
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Delivery {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let class = take_string(&mut fields, "class")?;
        let key = take_optional_string(&mut fields, "key")?;
        let revision = take_optional_number(&mut fields, "revision")?;
        Some(Self {
            class,
            key,
            revision,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(3 + self.extra.len());
        push_string(&mut fields, "class", self.class);
        if let Some(key) = self.key {
            push_string(&mut fields, "key", key);
        }
        if let Some(revision) = self.revision {
            fields.push(("revision".to_owned(), Value::Number(revision)));
        }
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Thread {
    /// Requests begin their correlation chain at their own message identifier,
    /// avoiding a second caller-chosen identity for the same interaction.
    pub fn for_request(thread_id: &str, request_id: &str) -> Self {
        Self {
            id: thread_id.to_owned(),
            correlation_id: request_id.to_owned(),
            causation_id: None,
            reply_to: None,
            extra: Vec::new(),
        }
    }

    /// Responses keep the request's correlation chain while naming the exact
    /// message that caused this reply, so transport acknowledgements cannot be
    /// confused with semantic response or acknowledgement messages.
    pub fn for_response(thread_id: &str, correlation_id: &str, causation_id: &str) -> Self {
        Self {
            id: thread_id.to_owned(),
            correlation_id: correlation_id.to_owned(),
            causation_id: Some(Value::String(causation_id.to_owned())),
            reply_to: None,
            extra: Vec::new(),
        }
    }

    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let id = take_string(&mut fields, "id")?;
        let correlation_id = take_string(&mut fields, "correlation_id")?;
        let causation_id = take(&mut fields, "causation_id");
        let reply_to = take(&mut fields, "reply_to");
        Some(Self {
            id,
            correlation_id,
            causation_id,
            reply_to,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(4 + self.extra.len());
        push_string(&mut fields, "id", self.id);
        push_string(&mut fields, "correlation_id", self.correlation_id);
        if let Some(causation_id) = self.causation_id {
            fields.push(("causation_id".to_owned(), causation_id));
        }
        if let Some(reply_to) = self.reply_to {
            fields.push(("reply_to".to_owned(), reply_to));
        }
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Context {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let org_id = take_string(&mut fields, "org_id")?;
        let project_id = take_string(&mut fields, "project_id")?;
        let repository_id = take_string(&mut fields, "repository_id")?;
        let git = match take(&mut fields, "git") {
            Some(Value::Null) | None => None,
            Some(value) => Some(GitContext::from_value(value)?),
        };
        let artifacts = match take(&mut fields, "artifacts") {
            Some(value) => into_array(value)?
                .into_iter()
                .map(Artifact::from_value)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self {
            org_id,
            project_id,
            repository_id,
            git,
            artifacts,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(5 + self.extra.len());
        push_string(&mut fields, "org_id", self.org_id);
        push_string(&mut fields, "project_id", self.project_id);
        push_string(&mut fields, "repository_id", self.repository_id);
        if let Some(git) = self.git {
            fields.push(("git".to_owned(), git.into_value()));
        }
        fields.push((
            "artifacts".to_owned(),
            Value::Array(Artifact::into_values(self.artifacts)),
        ));
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl GitContext {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let base_oid = take_optional_string(&mut fields, "base_oid")?;
        let plan_oid = take_optional_string(&mut fields, "plan_oid")?;
        let reference = take_optional_string(&mut fields, "ref")?;
        let commit = take_optional_string(&mut fields, "commit")?;
        Some(Self {
            base_oid,
            plan_oid,
            reference,
            commit,
            extra: fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(4 + self.extra.len());
        if let Some(base_oid) = self.base_oid {
            push_string(&mut fields, "base_oid", base_oid);
        }
        if let Some(plan_oid) = self.plan_oid {
            push_string(&mut fields, "plan_oid", plan_oid);
        }
        if let Some(reference) = self.reference {
            push_string(&mut fields, "ref", reference);
        }
        if let Some(commit) = self.commit {
            push_string(&mut fields, "commit", commit);
        }
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

impl Artifact {
    fn from_value(value: Value) -> Option<Self> {
        let mut fields = into_object(value)?;
        let kind = take_string(&mut fields, "kind")?;
        let id = take_string(&mut fields, "id")?;
        Some(Self {
            kind,
            id,
            extra: fields,
        })
    }

    fn into_values(artifacts: Vec<Self>) -> Vec<Value> {
        artifacts.into_iter().map(Self::into_value).collect()
    }

    fn into_value(self) -> Value {
        let mut fields = Vec::with_capacity(2 + self.extra.len());
        push_string(&mut fields, "kind", self.kind);
        push_string(&mut fields, "id", self.id);
        fields.extend(self.extra);
        Value::Object(fields)
    }
}

fn into_object(value: Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Object(fields) => Some(fields),
        _ => None,
    }
}

fn into_array(value: Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) => Some(items),
        _ => None,
    }
}

fn take(fields: &mut Vec<(String, Value)>, name: &str) -> Option<Value> {
    let index = fields.iter().position(|(field, _)| field == name)?;
    Some(fields.remove(index).1)
}

fn take_string(fields: &mut Vec<(String, Value)>, name: &str) -> Option<String> {
    match take(fields, name)? {
        Value::String(value) => Some(value),
        _ => None,
    }
}

fn take_optional_string(fields: &mut Vec<(String, Value)>, name: &str) -> Option<Option<String>> {
    match take(fields, name) {
        Some(Value::String(value)) => Some(Some(value)),
        Some(_) => None,
        None => Some(None),
    }
}

fn take_optional_number(fields: &mut Vec<(String, Value)>, name: &str) -> Option<Option<String>> {
    match take(fields, name) {
        Some(Value::Number(value)) => Some(Some(value)),
        Some(_) => None,
        None => Some(None),
    }
}

fn push_string(fields: &mut Vec<(String, Value)>, name: &str, value: String) {
    fields.push((name.to_owned(), Value::String(value)));
}

/// Parses and validates an untrusted frame without allocating lookup keys or
/// cloned field values during validation. Ownership is transferred only after
/// all structural, size, and expiry checks succeed.
pub fn validate(
    input: &[u8],
    topic: &str,
    principal: &AuthenticatedPrincipal<'_>,
    config: &ValidationConfig,
    now: DateTime<Utc>,
) -> Result<ValidatedEnvelope, Violation> {
    #[cfg(test)]
    let started =
        VALIDATION_PROFILE.with(|profile| profile.get().map(|_| std::time::Instant::now()));
    let verdict = validate_observed(input, topic, principal, config, now);
    #[cfg(test)]
    if let Some(started) = started {
        VALIDATION_PROFILE.with(|profile| {
            let (calls, nanoseconds) = profile.get().unwrap_or_default();
            profile.set(Some((
                calls + 1,
                nanoseconds + started.elapsed().as_nanos(),
            )));
        });
    }
    verdict
}

fn validate_observed(
    input: &[u8],
    topic: &str,
    principal: &AuthenticatedPrincipal<'_>,
    config: &ValidationConfig,
    now: DateTime<Utc>,
) -> Result<ValidatedEnvelope, Violation> {
    if input.len() > config.max_document_bytes {
        return Err(Violation::DocumentTooLarge);
    }
    let input = std::str::from_utf8(input).map_err(|_| Violation::InvalidUtf8)?;
    let value = json::parse(input).map_err(|_| Violation::InvalidJson)?;
    // The supplied principal is already authenticated. Locate only the
    // authority-bearing fields and bind them before the full structural pass,
    // so unauthorized senders do not receive a detailed structure oracle.
    validate_identity(&value, *principal)?;
    let checked = validate_structure(&value)?;
    validate_subject(&value)?;
    validate_type_and_schema(&value, config)?;
    if json_len(checked.payload) > config.max_payload_bytes {
        return Err(Violation::PayloadTooLarge);
    }
    let expires_at = DateTime::parse_from_rfc3339(checked.expires_at)
        .map_err(|_| Violation::InvalidExpiry)?
        .with_timezone(&Utc);
    if expires_at <= now {
        return Err(Violation::Expired);
    }
    if expires_at.signed_duration_since(now) > config.max_future_expiry {
        return Err(Violation::ExpiryTooFar);
    }
    validate_topic(&value, topic)?;
    validate_payload_and_anchors(&value)?;
    Envelope::from_value(value)
        .map(ValidatedEnvelope)
        .ok_or(Violation::InvalidEnvelopeShape)
}

struct CheckedStructure<'a> {
    payload: &'a Value,
    expires_at: &'a str,
}

fn validate_structure(value: &Value) -> Result<CheckedStructure<'_>, Violation> {
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    if has_duplicate_keys(value) {
        return Err(Violation::DuplicateJsonKey);
    }

    let specversion = field(outer, "specversion").ok_or(Violation::MissingSpecversion)?;
    if specversion.as_str() != Some("1.0") {
        return Err(Violation::WrongSpecversion);
    }
    let id = string_field(outer, "id").ok_or(Violation::MissingId)?;
    if id.is_empty() {
        return Err(Violation::MissingId);
    }
    if string_field(outer, "type").is_none_or(str::is_empty) {
        return Err(Violation::MissingType);
    }
    let datacontenttype =
        field(outer, "datacontenttype").ok_or(Violation::MissingDatacontenttype)?;
    if datacontenttype.as_str() != Some("application/json") {
        return Err(Violation::WrongDatacontenttype);
    }

    ensure_string(outer, "source")?;
    ensure_string(outer, "time")?;
    ensure_string(outer, "dataschema")?;
    if let Some(subject) = field(outer, "subject") {
        if subject.as_str().is_none() {
            return Err(Violation::InvalidEnvelopeShape);
        }
    }
    let data_value = field(outer, "data").ok_or(Violation::InvalidEnvelopeShape)?;
    let data = as_object(data_value).ok_or(Violation::InvalidEnvelopeShape)?;
    if outer
        .iter()
        .filter(|(name, _)| name != "data")
        .any(|(name, _)| data.iter().any(|(inner, _)| inner == name))
    {
        return Err(Violation::DuplicateEnvelopeField);
    }

    let intent = string_field(data, "intent").ok_or(Violation::InvalidIntent)?;
    let intent = Intent::from_wire(intent).ok_or(Violation::InvalidIntent)?;
    validate_producer(data)?;
    validate_recipients(data)?;
    let delivery_value = field(data, "delivery").ok_or(Violation::InvalidEnvelopeShape)?;
    let delivery = as_object(delivery_value).ok_or(Violation::InvalidEnvelopeShape)?;
    let delivery_class = string_field(delivery, "class").ok_or(Violation::InvalidDeliveryClass)?;
    match delivery_class {
        "event" | "inbox" => {}
        "latest-state" => {
            if string_field(delivery, "key").is_none() {
                return Err(Violation::MissingLatestStateKey);
            }
            if !matches!(field(delivery, "revision"), Some(Value::Number(_))) {
                return Err(Violation::MissingLatestStateRevision);
            }
        }
        _ => return Err(Violation::InvalidDeliveryClass),
    }
    validate_threading(id, intent, data)?;
    validate_context(data)?;
    let expires_at = string_field(data, "expires_at").ok_or(Violation::InvalidExpiry)?;
    ensure_string(data, "summary")?;
    if let Some(body) = field(data, "body") {
        if !matches!(body, Value::Null | Value::String(_)) {
            return Err(Violation::InvalidEnvelopeShape);
        }
    }
    let payload = field(data, "payload").ok_or(Violation::InvalidEnvelopeShape)?;

    Ok(CheckedStructure {
        payload,
        expires_at,
    })
}

fn validate_producer(data: &[(String, Value)]) -> Result<(), Violation> {
    let producer = field(data, "from")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    for name in [
        "principal_id",
        "agent_id",
        "instance_id",
        "display_name",
        "runtime",
    ] {
        if let Some(value) = field(producer, name) {
            if value.as_str().is_none() {
                return Err(Violation::InvalidEnvelopeShape);
            }
        }
    }
    Ok(())
}

fn validate_identity(
    value: &Value,
    principal: AuthenticatedPrincipal<'_>,
) -> Result<(), Violation> {
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    let source = string_field(outer, "source").ok_or(Violation::InvalidEnvelopeShape)?;
    let Some(source_instance) = source.strip_prefix("urn:loam:instance:") else {
        return Err(Violation::InvalidSourceScheme);
    };
    if source_instance.is_empty() {
        return Err(Violation::InvalidSourceScheme);
    }
    let data = field(outer, "data")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    let producer = field(data, "from")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    let principal_id = string_field(producer, "principal_id");
    let agent_id = string_field(producer, "agent_id");
    let instance_id = string_field(producer, "instance_id");
    if principal_id.is_none() || agent_id.is_none() || instance_id.is_none() {
        let payload_declares_identity =
            field(data, "payload")
                .and_then(as_object)
                .is_some_and(|payload| {
                    ["principal_id", "agent_id", "instance_id", "source"]
                        .iter()
                        .any(|name| field(payload, name).is_some())
                });
        if payload_declares_identity {
            return Err(Violation::PayloadDeclaredIdentity);
        }
        return Err(Violation::MissingStructuredIdentity);
    }
    let principal_id = principal_id.ok_or(Violation::MissingStructuredIdentity)?;
    let agent_id = agent_id.ok_or(Violation::MissingStructuredIdentity)?;
    let instance_id = instance_id.ok_or(Violation::MissingStructuredIdentity)?;
    if principal_id.is_empty() || agent_id.is_empty() || instance_id.is_empty() {
        return Err(Violation::MissingStructuredIdentity);
    }
    if source_instance != instance_id {
        return Err(Violation::SourceInstanceMismatch);
    }
    if !principal.can_claim(principal_id) {
        return Err(Violation::UnauthorizedPrincipal);
    }
    Ok(())
}

fn validate_subject(value: &Value) -> Result<(), Violation> {
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    if string_field(outer, "subject").is_some_and(is_plan_local_task_label) {
        Err(Violation::PlanLocalSubject)
    } else {
        Ok(())
    }
}

fn validate_type_and_schema(value: &Value, config: &ValidationConfig) -> Result<(), Violation> {
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    let message_type = string_field(outer, "type").ok_or(Violation::MissingType)?;
    let dataschema = string_field(outer, "dataschema").ok_or(Violation::InvalidEnvelopeShape)?;
    if let Some(expected) = core_schema(message_type) {
        if dataschema != expected {
            return Err(Violation::DataschemaMismatch);
        }
        return Ok(());
    }
    if message_type
        .as_bytes()
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"io.loam."))
    {
        return Err(Violation::ReservedType);
    }
    if !is_namespaced_type(message_type) {
        return Err(Violation::UnnamespacedType);
    }
    if schema_major(dataschema).is_none() {
        return Err(Violation::MissingSchemaMajor);
    }
    if let Some(binding) = config
        .extension_schemas
        .iter()
        .find(|binding| binding.message_type == message_type)
    {
        if binding.dataschema != dataschema {
            return Err(Violation::DataschemaMismatch);
        }
    }
    Ok(())
}

fn is_core_type(message_type: &str) -> bool {
    core_schema(message_type).is_some()
}

fn core_schema(message_type: &str) -> Option<&'static str> {
    match message_type {
        "io.loam.git.refs.changed" => Some("urn:loam:schema:git-refs-changed:1"),
        "io.loam.work.state" => Some("urn:loam:schema:work-state:1"),
        "io.loam.message" => Some("urn:loam:schema:message:1"),
        _ => None,
    }
}

fn is_namespaced_type(message_type: &str) -> bool {
    let mut segments = message_type.split('.');
    let mut count = 0;
    for segment in &mut segments {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn schema_major(dataschema: &str) -> Option<&str> {
    let major = dataschema.rsplit([':', '/']).next()?;
    if !major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(major)
    } else {
        None
    }
}

fn is_plan_local_task_label(subject: &str) -> bool {
    subject.strip_prefix('T').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub(crate) struct ParsedTopic<'a> {
    pub(crate) organization: &'a str,
    pub(crate) project: &'a str,
    pub(crate) delivery: TopicDelivery<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TopicDelivery<'a> {
    Event {
        origin: &'a str,
    },
    State {
        origin: &'a str,
        key: &'a str,
    },
    Inbox {
        recipient_kind: &'a str,
        recipient: &'a str,
        origin: &'a str,
        message_id: &'a str,
    },
    /// The broker-served project membership topic (`membership` class): a
    /// retained payload carrying the roster JSON. Not a loam envelope — the
    /// connector writes it to the local roster file verbatim.
    Membership,
    /// The self-announced member-card topic
    /// (`loam/v1/{org}/members/{instance_id}`): a retained payload carrying one
    /// member's card JSON. Not a loam envelope — the connector writes it to the
    /// local member-card cache and reassembles project rosters from the set.
    /// Org-scoped, so `project` carries the literal `members` marker and
    /// `instance_id` names which card this is.
    MemberCard {
        instance_id: &'a str,
    },
}

impl TopicDelivery<'_> {
    pub(crate) fn origin(&self) -> &str {
        match self {
            Self::Event { origin } | Self::State { origin, .. } | Self::Inbox { origin, .. } => {
                origin
            }
            // A membership frame has no origin; the membership ACL grants
            // read on the topic, not a per-instance origin write. A member
            // card is similarly broker-served: the card's own instance id is in
            // the topic, and read is granted by the members/+ ACL, not by an
            // origin claim on the frame.
            Self::Membership | Self::MemberCard { .. } => "",
        }
    }

    pub(crate) fn envelope_class(&self) -> &str {
        match self {
            Self::Event { .. } => "event",
            Self::State { .. } => "latest-state",
            Self::Inbox { .. } => "inbox",
            Self::Membership => "membership",
            Self::MemberCard { .. } => "members",
        }
    }
}

pub(crate) fn parse_topic(topic: &str) -> Result<ParsedTopic<'_>, Violation> {
    if topic.len() > MAX_MQTT_TOPIC_BYTES || topic.contains('\0') {
        return Err(Violation::MalformedTopic);
    }
    let mut segments = topic.split('/');
    if segments.next() != Some("loam") || segments.next() != Some("v1") {
        return Err(Violation::MalformedTopic);
    }
    let organization = segments.next().filter(|value| valid_topic_segment(value));
    let Some(organization) = organization else {
        return Err(Violation::MalformedTopic);
    };

    // The self-announced member-card topic is org-scoped: the project slot is
    // the literal `members` marker followed by the instance id, with no
    // project and no further segments.
    match segments.next() {
        Some("members") => {
            let instance_id = segments.next().filter(|value| valid_topic_segment(value));
            let Some(instance_id) = instance_id else {
                return Err(Violation::MalformedTopic);
            };
            if segments.next().is_some() {
                return Err(Violation::MalformedTopic);
            }
            return Ok(ParsedTopic {
                organization,
                project: "members",
                delivery: TopicDelivery::MemberCard { instance_id },
            });
        }
        Some(_) => {}
        None => return Err(Violation::MalformedTopic),
    }

    // Re-walk from the project slot for the project-scoped classes.
    let mut segments = topic.split('/');
    let _ = segments.next(); // loam
    let _ = segments.next(); // v1
    let _ = segments.next(); // organization
    let project = segments.next().filter(|value| valid_topic_segment(value));
    let class = segments.next();
    let (Some(project), Some(class)) = (project, class) else {
        return Err(Violation::MalformedTopic);
    };
    let delivery = match class {
        "event" => {
            let origin = segments.next().filter(|value| valid_topic_segment(value));
            let Some(origin) = origin else {
                return Err(Violation::MalformedTopic);
            };
            TopicDelivery::Event { origin }
        }
        "state" => {
            let origin = segments.next().filter(|value| valid_topic_segment(value));
            let key = segments.next().filter(|value| valid_topic_segment(value));
            let (Some(origin), Some(key)) = (origin, key) else {
                return Err(Violation::MalformedTopic);
            };
            TopicDelivery::State { origin, key }
        }
        "inbox" => {
            let remaining = segments.by_ref().collect::<Vec<_>>();
            let [recipient_kind, recipient, origin, message_id] = remaining.as_slice() else {
                return Err(Violation::MalformedTopic);
            };
            if ![*recipient_kind, *recipient, *origin, *message_id]
                .into_iter()
                .all(valid_topic_segment)
            {
                return Err(Violation::MalformedTopic);
            }
            if !matches!(*recipient_kind, "agent" | "principal" | "instance") {
                return Err(Violation::UnknownRecipientKind);
            }
            TopicDelivery::Inbox {
                recipient_kind,
                recipient,
                origin,
                message_id,
            }
        }
        // The broker-served membership topic carries no further segments: the
        // payload is the roster JSON for (org, project).
        "membership" => TopicDelivery::Membership,
        _ => return Err(Violation::MalformedTopic),
    };
    if segments.next().is_some() {
        return Err(Violation::MalformedTopic);
    }
    Ok(ParsedTopic {
        organization,
        project,
        delivery,
    })
}

fn valid_topic_segment(segment: &str) -> bool {
    !segment.is_empty() && !segment.contains(['+', '#', '\0'])
}

fn validate_topic(value: &Value, topic: &str) -> Result<(), Violation> {
    let topic = parse_topic(topic)?;
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    let data = field(outer, "data")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    let context = field(data, "context")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if string_field(context, "org_id") != Some(topic.organization) {
        return Err(Violation::BindingMismatch(BindingAxis::Organization));
    }
    if string_field(context, "project_id") != Some(topic.project) {
        return Err(Violation::BindingMismatch(BindingAxis::Project));
    }
    let producer = field(data, "from")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if string_field(producer, "instance_id") != Some(topic.delivery.origin()) {
        return Err(Violation::BindingMismatch(BindingAxis::Origin));
    }
    let delivery = field(data, "delivery")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if string_field(delivery, "class") != Some(topic.delivery.envelope_class()) {
        return Err(Violation::BindingMismatch(BindingAxis::DeliveryClass));
    }
    let recipients = field(data, "to")
        .and_then(Value::as_array)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if recipients.is_empty() {
        return Err(Violation::BindingMismatch(BindingAxis::Audience));
    }
    let inbox_recipient = match &topic.delivery {
        TopicDelivery::Inbox {
            recipient_kind,
            recipient,
            ..
        } => Some((*recipient_kind, *recipient)),
        TopicDelivery::Event { .. } | TopicDelivery::State { .. } => None,
        // A membership or member-card frame is never an envelope; it is
        // refused before the envelope validator by the transport's
        // membership read-path.
        TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => None,
    };
    let mut directly_addressed = false;
    for recipient in recipients {
        let recipient = as_object(recipient).ok_or(Violation::InvalidEnvelopeShape)?;
        let kind = string_field(recipient, "kind").ok_or(Violation::InvalidEnvelopeShape)?;
        let id = string_field(recipient, "id").ok_or(Violation::InvalidEnvelopeShape)?;
        match kind {
            "org" if id != topic.organization => {
                return Err(Violation::BindingMismatch(BindingAxis::Audience));
            }
            "org" => {}
            "project" if id != topic.project => {
                return Err(Violation::BindingMismatch(BindingAxis::Audience));
            }
            "project" => {}
            "agent" | "principal" | "instance" => {
                let Some((expected_kind, expected_id)) = inbox_recipient else {
                    return Err(Violation::BindingMismatch(BindingAxis::Audience));
                };
                if kind != expected_kind {
                    return Err(Violation::BindingMismatch(BindingAxis::RecipientKind));
                }
                if id != expected_id {
                    return Err(Violation::BindingMismatch(BindingAxis::Recipient));
                }
                directly_addressed = true;
            }
            _ => return Err(Violation::UnknownRecipientKind),
        }
    }
    match topic.delivery {
        TopicDelivery::Event { .. } => {}
        TopicDelivery::State { key, .. } => {
            if string_field(delivery, "key") != Some(key) {
                return Err(Violation::BindingMismatch(BindingAxis::StateKey));
            }
        }
        TopicDelivery::Inbox { message_id, .. } => {
            if !directly_addressed {
                return Err(Violation::BindingMismatch(BindingAxis::Recipient));
            }
            if string_field(outer, "id") != Some(message_id) {
                return Err(Violation::BindingMismatch(BindingAxis::MessageId));
            }
        }
        TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => {}
    }
    Ok(())
}

fn validate_recipients(data: &[(String, Value)]) -> Result<(), Violation> {
    let recipients = field(data, "to")
        .and_then(Value::as_array)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    for recipient in recipients {
        let recipient = as_object(recipient).ok_or(Violation::InvalidEnvelopeShape)?;
        ensure_string(recipient, "kind")?;
        ensure_string(recipient, "id")?;
    }
    Ok(())
}

fn validate_threading(
    message_id: &str,
    intent: Intent,
    data: &[(String, Value)],
) -> Result<(), Violation> {
    let thread = match field(data, "thread") {
        Some(Value::Null) | None => None,
        Some(value) => Some(as_object(value).ok_or(Violation::InvalidEnvelopeShape)?),
    };
    if let Some(thread) = thread {
        ensure_string(thread, "id")?;
        ensure_string(thread, "correlation_id")?;
        validate_optional_identifier(thread, "causation_id")?;
        validate_optional_identifier(thread, "reply_to")?;
    }
    match intent {
        Intent::Request => {
            let thread = thread.ok_or(Violation::MissingRequestThread)?;
            if string_field(thread, "correlation_id") != Some(message_id) {
                return Err(Violation::InvalidRequestCorrelation);
            }
        }
        Intent::Response => {
            let thread = thread.ok_or(Violation::MissingResponseThread)?;
            let has_causation = ["causation_id", "reply_to"].iter().any(|name| {
                string_field(thread, name).is_some_and(|identifier| !identifier.is_empty())
            });
            if !has_causation {
                return Err(Violation::MissingResponseCausation);
            }
        }
        Intent::Inform | Intent::Ack => {}
    }
    Ok(())
}

fn validate_optional_identifier(fields: &[(String, Value)], name: &str) -> Result<(), Violation> {
    if let Some(value) = field(fields, name) {
        if !matches!(value, Value::Null | Value::String(_)) {
            return Err(Violation::InvalidEnvelopeShape);
        }
    }
    Ok(())
}

fn validate_context(data: &[(String, Value)]) -> Result<(), Violation> {
    let context = field(data, "context")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if has_unknown_field(
        context,
        &["org_id", "project_id", "repository_id", "git", "artifacts"],
    ) {
        return Err(Violation::UnknownContextField);
    }
    ensure_string(context, "org_id")?;
    ensure_string(context, "project_id")?;
    if let Some(repository_id) = field(context, "repository_id") {
        if repository_id.as_str().is_none() {
            return Err(Violation::InvalidEnvelopeShape);
        }
    }
    if let Some(git) = field(context, "git") {
        if !matches!(git, Value::Null) {
            let git = as_object(git).ok_or(Violation::InvalidEnvelopeShape)?;
            if has_unknown_field(git, &["base_oid", "plan_oid", "ref", "commit"]) {
                return Err(Violation::UnknownContextField);
            }
            for name in ["base_oid", "plan_oid", "ref", "commit"] {
                if let Some(value) = field(git, name) {
                    if value.as_str().is_none() {
                        return Err(Violation::InvalidEnvelopeShape);
                    }
                }
            }
        }
    }
    let artifacts = field(context, "artifacts")
        .and_then(Value::as_array)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    for artifact in artifacts {
        let artifact = as_object(artifact).ok_or(Violation::InvalidEnvelopeShape)?;
        if has_unknown_field(artifact, &["kind", "id"]) {
            return Err(Violation::UnknownContextField);
        }
        ensure_string(artifact, "kind")?;
        if let Some(id) = field(artifact, "id") {
            if id.as_str().is_none() {
                return Err(Violation::InvalidEnvelopeShape);
            }
        }
    }
    Ok(())
}

fn has_unknown_field(fields: &[(String, Value)], allowed: &[&str]) -> bool {
    fields
        .iter()
        .any(|(name, _)| !allowed.contains(&name.as_str()))
}

fn validate_payload_and_anchors(value: &Value) -> Result<(), Violation> {
    let outer = as_object(value).ok_or(Violation::InvalidEnvelopeShape)?;
    let message_type = string_field(outer, "type").ok_or(Violation::MissingType)?;
    let data = field(outer, "data")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    let context = field(data, "context")
        .and_then(as_object)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    if string_field(context, "repository_id").is_none_or(str::is_empty) {
        return Err(Violation::MissingRepositoryId);
    }
    let payload = field(data, "payload").ok_or(Violation::InvalidEnvelopeShape)?;
    let git = field(context, "git").and_then(as_object);
    validate_supplied_git_anchors(git)?;

    match message_type {
        "io.loam.git.refs.changed" => validate_git_refs_payload(payload),
        "io.loam.work.state" => validate_work_state(payload, context, git),
        "io.loam.message" => validate_message_payload(payload, git),
        _ => Ok(()),
    }
}

fn validate_supplied_git_anchors(git: Option<&[(String, Value)]>) -> Result<(), Violation> {
    let Some(git) = git else {
        return Ok(());
    };
    if let Some(base_oid) = string_field(git, "base_oid") {
        if !valid_git_oid(base_oid) {
            return Err(Violation::InvalidBaseOid);
        }
    }
    if let Some(plan_oid) = string_field(git, "plan_oid") {
        if !valid_git_oid(plan_oid) {
            return Err(Violation::InvalidPlanOid);
        }
    }
    if let Some(commit) = string_field(git, "commit") {
        if !valid_git_oid(commit) {
            return Err(Violation::InvalidReviewCommit);
        }
    }
    Ok(())
}

fn validate_git_refs_payload(payload: &Value) -> Result<(), Violation> {
    let payload = as_object(payload).ok_or(Violation::InvalidGitRefsPayload)?;
    let old_oid = string_field(payload, "old_oid").ok_or(Violation::MissingOldOid)?;
    if !valid_git_oid(old_oid) {
        return Err(Violation::InvalidOldOid);
    }
    let new_oid = string_field(payload, "new_oid").ok_or(Violation::MissingNewOid)?;
    if !valid_git_oid(new_oid) {
        return Err(Violation::InvalidNewOid);
    }
    Ok(())
}

fn validate_work_state(
    payload: &Value,
    context: &[(String, Value)],
    git: Option<&[(String, Value)]>,
) -> Result<(), Violation> {
    let payload = as_object(payload).ok_or(Violation::InvalidWorkStatePayload)?;
    let state = string_field(payload, "state").ok_or(Violation::InvalidWorkState)?;
    if !matches!(
        state,
        "active" | "blocked" | "ready" | "published" | "abandoned"
    ) {
        return Err(Violation::InvalidWorkState);
    }
    let git = git.ok_or(Violation::MissingBaseOid)?;
    if string_field(git, "base_oid").is_none() {
        return Err(Violation::MissingBaseOid);
    }

    let artifacts = field(context, "artifacts")
        .and_then(Value::as_array)
        .ok_or(Violation::InvalidEnvelopeShape)?;
    let mut has_claim = false;
    for artifact in artifacts {
        let artifact = as_object(artifact).ok_or(Violation::InvalidEnvelopeShape)?;
        let kind = string_field(artifact, "kind").ok_or(Violation::InvalidEnvelopeShape)?;
        let identifier = string_field(artifact, "id")
            .filter(|identifier| !identifier.is_empty() && !is_plan_local_task_label(identifier))
            .ok_or(Violation::MissingStableArtifactId)?;
        let _ = identifier;
        if matches!(kind, "task" | "acceptance") {
            has_claim = true;
        }
    }
    if let Some(acceptance) = field(payload, "acceptance") {
        let acceptance = as_object(acceptance).ok_or(Violation::InvalidWorkStatePayload)?;
        for (identifier, verdict) in acceptance {
            if identifier.is_empty()
                || is_plan_local_task_label(identifier)
                || verdict.as_str().is_none_or(str::is_empty)
            {
                return Err(Violation::MissingStableArtifactId);
            }
        }
        has_claim |= !acceptance.is_empty();
    }
    if let Some(verification) = field(payload, "verification") {
        let verification = verification
            .as_array()
            .ok_or(Violation::InvalidWorkStatePayload)?;
        if verification
            .iter()
            .any(|claim| claim.as_str().is_none_or(str::is_empty))
        {
            return Err(Violation::InvalidWorkStatePayload);
        }
    }
    if has_claim && string_field(git, "plan_oid").is_none() {
        return Err(Violation::MissingPlanOid);
    }
    Ok(())
}

fn validate_message_payload(
    payload: &Value,
    git: Option<&[(String, Value)]>,
) -> Result<(), Violation> {
    let payload = as_object(payload).ok_or(Violation::InvalidMessagePayload)?;
    let action = string_field(payload, "action")
        .filter(|action| !action.is_empty())
        .ok_or(Violation::MissingMessageAction)?;
    if field(payload, "params").and_then(as_object).is_none() {
        return Err(Violation::InvalidMessageParams);
    }
    if let Some(status) = field(payload, "response_status") {
        if !matches!(status, Value::Null | Value::String(_)) {
            return Err(Violation::InvalidResponseStatus);
        }
    }
    if action == "review.request" {
        let Some(git) = git else {
            return Err(Violation::MissingReviewAnchor);
        };
        let has_reference = string_field(git, "ref").is_some_and(|value| !value.is_empty());
        let has_commit = string_field(git, "commit").is_some_and(|value| !value.is_empty());
        if !has_reference && !has_commit {
            return Err(Violation::MissingReviewAnchor);
        }
    }
    Ok(())
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ensure_string(fields: &[(String, Value)], name: &str) -> Result<(), Violation> {
    if string_field(fields, name).is_some() {
        Ok(())
    } else {
        Err(Violation::InvalidEnvelopeShape)
    }
}

fn field<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    fields
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
}

fn string_field<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    field(fields, name).and_then(Value::as_str)
}

fn as_object(value: &Value) -> Option<&[(String, Value)]> {
    match value {
        Value::Object(fields) => Some(fields),
        _ => None,
    }
}

fn has_duplicate_keys(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            // ponytail: quadratic within one object avoids allocating a key set;
            // switch to reusable hash scratch if wide payload objects become common.
            fields
                .iter()
                .enumerate()
                .any(|(index, (name, _))| fields[index + 1..].iter().any(|(next, _)| next == name))
                || fields.iter().any(|(_, value)| has_duplicate_keys(value))
        }
        Value::Array(items) => items.iter().any(has_duplicate_keys),
        _ => false,
    }
}

fn json_len(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.len(),
        Value::String(text) => json_string_len(text),
        Value::Array(items) => {
            2 + items.iter().map(json_len).sum::<usize>() + items.len().saturating_sub(1)
        }
        Value::Object(fields) => {
            2 + fields
                .iter()
                .map(|(name, value)| json_string_len(name) + 1 + json_len(value))
                .sum::<usize>()
                + fields.len().saturating_sub(1)
        }
    }
}

fn json_string_len(value: &str) -> usize {
    2 + value
        .chars()
        .map(|character| match character {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        })
        .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use chrono::{DateTime, Duration, Utc};

    const CORE_FIXTURES: &[&str] = &[
        include_str!("../tests/fixtures/mqtt/git-refs-changed.json"),
        include_str!("../tests/fixtures/mqtt/work-state.json"),
        include_str!("../tests/fixtures/mqtt/message.json"),
    ];

    #[test]
    fn a_violation_names_itself_in_a_stable_grep_token() {
        // The token is what reaches an operator: an IPC `diagnostic`, a delivery
        // breadcrumb, `federation emit`'s refusal line (#102). Pinned by value,
        // because the point of the code is that it does not move under a caller
        // who greps for it.
        assert_eq!(Violation::MissingPlanOid.code(), "missing_plan_oid");
        assert_eq!(Violation::Expired.code(), "expired");
        assert_eq!(Violation::InvalidUtf8.code(), "invalid_utf8");
        assert_eq!(
            Violation::MissingLatestStateRevision.code(),
            "missing_latest_state_revision"
        );

        // The one payload-carrying variant keeps its axis, and the axis whose
        // name is a caller-chosen state key still contributes only a literal.
        assert_eq!(
            Violation::BindingMismatch(BindingAxis::StateKey).code(),
            "binding_mismatch:state_key"
        );
        assert_eq!(
            Violation::BindingMismatch(BindingAxis::DeliveryClass).code(),
            "binding_mismatch:delivery_class"
        );
    }

    /// Every violation this crate can produce has a token-shaped code — a
    /// refusal that reached a log as an empty string or a sentence would be
    /// worse than the flattened `invalid_request` it replaced.
    #[test]
    fn every_violation_a_real_envelope_can_hit_has_a_token_shaped_code() {
        // Driven through the real validator rather than a hand-listed enum:
        // whatever these malformed documents refuse, the code for it must be a
        // token. `id` and `type` cover the parse-level rules, the work-state
        // fixtures the payload-level ones.
        let malformed: [&[u8]; 5] = [
            b"not json at all",
            b"{}",
            br#"{"specversion":"1.0"}"#,
            &[0xff, 0xfe, 0xfd],
            br#"{"specversion":"1.0","id":"e-1","type":"io.loam.work.state"}"#,
        ];
        let principal = AuthenticatedPrincipal::new("employee-42", &["employee-42"]);
        for document in malformed {
            let violation = validate(
                document,
                "loam/v1/acme/loam/state/instance-01/task-7",
                &principal,
                &ValidationConfig::default(),
                Utc::now(),
            )
            .expect_err("a malformed document must be refused");
            let code = violation.code();
            assert!(!code.is_empty(), "{violation:?} has no code");
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ':'),
                "a code is a grep token, not prose: {code}"
            );
        }
    }

    #[test]
    fn core_fixtures_round_trip_without_loss() {
        for fixture in CORE_FIXTURES {
            let original = json::parse(fixture).expect("golden fixture should be valid JSON");
            let message = Envelope::from_value(original.clone())
                .expect("golden fixture should have the envelope shape");

            assert_eq!(message.into_value(), original);
        }
    }

    #[test]
    fn validate_rejects_structural_cases_with_specific_violations() {
        let corpus = json::parse(include_str!(
            "../tests/fixtures/mqtt/malformed-envelope-cases.json"
        ))
        .expect("malformed-envelope corpus should be valid JSON");

        for case in corpus
            .as_array()
            .expect("malformed-envelope corpus should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let base = case
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("work-state");
            let mut message = json::parse(fixture(base)).expect("base fixture should parse");
            if let Some(patches) = case.get("patches").and_then(Value::as_array) {
                for patch in patches {
                    apply_fixture_patch(&mut message, patch);
                }
            }

            let mut config = ValidationConfig::default();
            if let Some(maximum) = case
                .get("max_payload_bytes")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
            {
                config.max_payload_bytes = maximum;
            }
            if let Some(maximum) = case
                .get("max_document_bytes")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
            {
                config.max_document_bytes = maximum;
            }
            if let Some(seconds) = case
                .get("max_future_seconds")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
            {
                config.max_future_expiry = Duration::seconds(seconds);
            }
            let now = case
                .get("now")
                .and_then(Value::as_str)
                .unwrap_or("2026-07-24T14:21:00Z");
            let now = DateTime::parse_from_rfc3339(now)
                .expect("fixture time should be RFC 3339")
                .with_timezone(&Utc);
            let input = message.to_json();

            assert_eq!(
                validate(
                    input.as_bytes(),
                    topic_for("work-state"),
                    &authenticated_principal(),
                    &config,
                    now,
                ),
                Err(expected_violation(name)),
                "case {name}"
            );
        }
    }

    #[test]
    fn validate_rejects_invalid_json_and_utf8_before_structure() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        assert_eq!(
            validate(
                include_bytes!("../tests/fixtures/mqtt/invalid-json.json"),
                topic_for("work-state"),
                &authenticated_principal(),
                &config,
                now
            ),
            Err(Violation::InvalidJson)
        );
        let invalid_utf8 = u8::from_str_radix(
            include_str!("../tests/fixtures/mqtt/invalid-utf8.hex").trim(),
            16,
        )
        .expect("hex fixture should decode");
        assert_eq!(
            validate(
                &[invalid_utf8],
                topic_for("work-state"),
                &authenticated_principal(),
                &config,
                now,
            ),
            Err(Violation::InvalidUtf8)
        );

        let mut bounded = config.clone();
        bounded.max_document_bytes = 1;
        assert_eq!(
            validate(
                include_bytes!("../tests/fixtures/mqtt/invalid-json.json"),
                topic_for("work-state"),
                &authenticated_principal(),
                &bounded,
                now,
            ),
            Err(Violation::DocumentTooLarge)
        );
    }

    #[test]
    fn identity_rejects_each_binding_case_with_specific_violations() {
        let cases = json::parse(include_str!(
            "../tests/fixtures/mqtt/identity-binding-cases.json"
        ))
        .expect("identity corpus should be valid JSON");
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        for case in cases
            .as_array()
            .expect("identity corpus should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let mut message =
                json::parse(fixture("work-state")).expect("base fixture should parse");
            for patch in case
                .get("patches")
                .and_then(Value::as_array)
                .expect("case should have patches")
            {
                apply_fixture_patch(&mut message, patch);
            }
            let input = message.to_json();

            assert_eq!(
                validate(
                    input.as_bytes(),
                    topic_for("work-state"),
                    &authenticated_principal(),
                    &config,
                    now,
                ),
                Err(expected_identity_violation(name)),
                "case {name}"
            );
        }
    }

    #[test]
    fn identity_runtime_is_diagnostic_only_for_every_verdict() {
        for authorized in [true, false] {
            let mut verdicts = Vec::new();
            for runtime in [Some("claude"), Some("opencode"), None, Some("")] {
                let mut message = json::parse(fixture("work-state")).expect("fixture should parse");
                if !authorized {
                    apply_fixture_patch(
                        &mut message,
                        &json::parse(
                            r#"{"op":"set","path":["data","from","principal_id"],"value":"employee-999"}"#,
                        )
                        .expect("patch should parse"),
                    );
                }
                set_runtime(&mut message, runtime);
                verdicts.push(validate_identity(&message, authenticated_principal()));
            }
            let expected = if authorized {
                Ok(())
            } else {
                Err(Violation::UnauthorizedPrincipal)
            };
            assert!(
                verdicts.iter().all(|verdict| *verdict == expected),
                "runtime changed the identity verdict"
            );
        }

        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let validated = validate(
            fixture("work-state").as_bytes(),
            topic_for("work-state"),
            &authenticated_principal(),
            &config,
            now,
        )
        .expect("valid runtime should be retained");
        assert_eq!(
            validated.as_envelope().data.from.runtime.as_deref(),
            Some("opencode")
        );
    }

    #[test]
    fn the_display_name_round_trips_and_is_never_written_twice() {
        // The display name is the authenticated certificate's given name. It is
        // provenance, never authority, so it follows `runtime`: optional on the
        // wire, absent when absent, and never defaulted into existence.
        let mut root = crate::json::parse(fixture("work-state")).expect("fixture parses");
        set_display_name(&mut root, Some("Ada Lovelace"));
        let document = root.to_json();

        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let validated = validate(
            document.as_bytes(),
            topic_for("work-state"),
            &authenticated_principal(),
            &config,
            now,
        )
        .expect("a display name must not change the verdict");
        assert_eq!(
            validated.as_envelope().data.from.display_name.as_deref(),
            Some("Ada Lovelace")
        );

        // Typed, not carried in `extra`: a field read into both would serialize
        // twice and the second copy would win at the far end.
        let written = validated.as_envelope().to_json();
        assert_eq!(
            written.matches("\"display_name\"").count(),
            1,
            "the display name must be written exactly once: {written}"
        );

        // Control: absent stays absent rather than becoming an empty string.
        let absent = validate(
            fixture("work-state").as_bytes(),
            topic_for("work-state"),
            &authenticated_principal(),
            &config,
            now,
        )
        .expect("the unmodified fixture still validates");
        assert!(absent.as_envelope().data.from.display_name.is_none());
        assert!(!absent.as_envelope().to_json().contains("display_name"));
    }

    #[test]
    fn a_non_string_display_name_is_refused_like_every_other_identity_field() {
        // It is sender-supplied text, so its shape is validated with the rest of
        // the producer rather than trusted because it is only cosmetic.
        let mut root = crate::json::parse(fixture("work-state")).expect("fixture parses");
        set_display_name_value(&mut root, Some(Value::Number("7".into())));
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        assert!(validate(
            root.to_json().as_bytes(),
            topic_for("work-state"),
            &authenticated_principal(),
            &config,
            now,
        )
        .is_err());
    }

    #[test]
    fn identity_subject_is_optional_and_never_defaulted() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let validated = validate(
            fixture("work-state").as_bytes(),
            topic_for("work-state"),
            &authenticated_principal(),
            &config,
            now,
        )
        .expect("fixture without subject should validate");

        assert_eq!(validated.as_envelope().subject, None);
    }

    #[test]
    fn binding_accepts_topics_for_all_core_delivery_shapes() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        for base in ["git-refs-changed", "work-state", "message"] {
            assert!(
                validate(
                    fixture(base).as_bytes(),
                    topic_for(base),
                    &authenticated_principal(),
                    &config,
                    now,
                )
                .is_ok(),
                "base {base}"
            );
        }

        let cases = json::parse(include_str!(
            "../tests/fixtures/mqtt/topic-recipient-kind-cases.json"
        ))
        .expect("recipient-kind corpus should parse");
        for case in cases
            .as_array()
            .expect("recipient-kind corpus should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let expected = case
                .get("expected")
                .and_then(Value::as_str)
                .expect("case should have an expected verdict");
            let base = case
                .get("base")
                .and_then(Value::as_str)
                .expect("case should have a base");
            let topic = case
                .get("topic")
                .and_then(Value::as_str)
                .expect("case should have a topic");
            let mut message = json::parse(fixture(base)).expect("base fixture should parse");
            for patch in case
                .get("patches")
                .and_then(Value::as_array)
                .expect("case should have patches")
            {
                apply_fixture_patch(&mut message, patch);
            }
            let verdict = validate(
                message.to_json().as_bytes(),
                topic,
                &authenticated_principal(),
                &config,
                now,
            );
            let expected = match expected {
                "ok" => Ok(()),
                "recipient_kind_mismatch" => {
                    Err(Violation::BindingMismatch(BindingAxis::RecipientKind))
                }
                "unknown_recipient_kind" => Err(Violation::UnknownRecipientKind),
                "malformed_topic" => Err(Violation::MalformedTopic),
                _ => panic!("unmapped recipient-kind verdict {expected}"),
            };
            assert_eq!(verdict.map(|_| ()), expected, "case {name}");
        }
    }

    #[test]
    fn binding_rejects_each_topic_disagreement_without_normalizing() {
        let cases = json::parse(include_str!(
            "../tests/fixtures/mqtt/topic-binding-cases.json"
        ))
        .expect("binding corpus should be valid JSON");
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        for case in cases.as_array().expect("binding corpus should be an array") {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let base = case
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("work-state");
            let topic = case
                .get("topic")
                .and_then(Value::as_str)
                .expect("case should have a topic");
            let mut message = json::parse(fixture(base)).expect("base fixture should parse");
            if let Some(patches) = case.get("patches").and_then(Value::as_array) {
                for patch in patches {
                    apply_fixture_patch(&mut message, patch);
                }
            }

            assert_eq!(
                validate(
                    message.to_json().as_bytes(),
                    topic,
                    &authenticated_principal(),
                    &config,
                    now,
                ),
                Err(expected_binding_violation(name)),
                "case {name}"
            );
        }
    }

    #[test]
    fn binding_rejects_mqtt_topic_protocol_limits() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let topic = format!("loam/v1/org-3A1/project-7M3/event/{}", "a".repeat(65_536));

        assert_eq!(
            validate(
                fixture("git-refs-changed").as_bytes(),
                &topic,
                &authenticated_principal(),
                &config,
                now,
            ),
            Err(Violation::MalformedTopic)
        );
    }

    #[test]
    fn the_member_card_topic_is_org_scoped_with_a_members_marker() {
        // `loam/v1/{org}/members/{instance_id}`: the project slot carries the
        // literal `members` marker and the instance id follows, with no project
        // segment and nothing after the instance.
        let parsed = parse_topic("loam/v1/acme/members/01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("a well-formed member-card topic parses");
        assert_eq!(parsed.organization, "acme");
        assert_eq!(parsed.project, "members");
        assert_eq!(
            parsed.delivery,
            TopicDelivery::MemberCard {
                instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            }
        );

        // A missing instance, an extra segment, and a wildcard id are all
        // malformed.
        for bad in [
            "loam/v1/acme/members",
            "loam/v1/acme/members/",
            "loam/v1/acme/members/01ARZ3NDEKTSV4RRFFQ69G5FAV/extra",
            "loam/v1/acme/members/+", // wildcard is not a valid card id
        ] {
            assert_eq!(
                parse_topic(bad).map(|_| ()),
                Err(Violation::MalformedTopic),
                "topic {bad}"
            );
        }
    }

    #[test]
    fn payload_rejects_each_schema_and_anchor_case_specifically() {
        let cases = json::parse(include_str!(
            "../tests/fixtures/mqtt/payload-anchor-cases.json"
        ))
        .expect("payload corpus should be valid JSON");
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        for case in cases.as_array().expect("payload corpus should be an array") {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let base = case
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("work-state");
            let mut message = json::parse(fixture(base)).expect("base fixture should parse");
            for patch in case
                .get("patches")
                .and_then(Value::as_array)
                .expect("case should have patches")
            {
                apply_fixture_patch(&mut message, patch);
            }

            assert_eq!(
                validate(
                    message.to_json().as_bytes(),
                    topic_for(base),
                    &authenticated_principal(),
                    &config,
                    now,
                ),
                Err(expected_payload_violation(name)),
                "case {name}"
            );
        }
    }

    #[test]
    fn payload_accepts_every_work_state_value_and_open_message_action() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        for state in ["active", "blocked", "ready", "published", "abandoned"] {
            let mut message = json::parse(fixture("work-state")).expect("fixture should parse");
            apply_fixture_patch(
                &mut message,
                &Value::Object(vec![
                    ("op".to_owned(), Value::String("set".to_owned())),
                    (
                        "path".to_owned(),
                        Value::Array(vec![
                            Value::String("data".to_owned()),
                            Value::String("payload".to_owned()),
                            Value::String("state".to_owned()),
                        ]),
                    ),
                    ("value".to_owned(), Value::String(state.to_owned())),
                ]),
            );
            assert!(validate(
                message.to_json().as_bytes(),
                topic_for("work-state"),
                &authenticated_principal(),
                &config,
                now,
            )
            .is_ok());
        }

        let mut message = json::parse(fixture("message")).expect("fixture should parse");
        apply_fixture_patch(
            &mut message,
            &json::parse(
                r#"{"op":"set","path":["data","payload","action"],"value":"org.example.question"}"#,
            )
            .expect("patch should parse"),
        );
        assert!(validate(
            message.to_json().as_bytes(),
            topic_for("message"),
            &authenticated_principal(),
            &config,
            now,
        )
        .is_ok());
    }

    #[test]
    fn payload_core_types_remain_validated_data_without_effects() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        for base in ["git-refs-changed", "work-state", "message"] {
            let original = json::parse(fixture(base)).expect("fixture should parse");
            let original_payload = original
                .get("data")
                .and_then(|data| data.get("payload"))
                .expect("fixture should have payload");
            let validated = validate(
                fixture(base).as_bytes(),
                topic_for(base),
                &authenticated_principal(),
                &config,
                now,
            )
            .expect("core fixture should validate");

            assert_eq!(&validated.as_envelope().data.payload, original_payload);
        }
    }

    #[test]
    fn extension_preserves_unknown_payload_and_exposes_display_fields() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let original = json::parse(fixture("extension")).expect("extension fixture should parse");
        let validated = validate(
            fixture("extension").as_bytes(),
            topic_for("extension"),
            &authenticated_principal(),
            &config,
            now,
        )
        .expect("namespaced extension should validate");
        let view = validated
            .extension_view()
            .expect("unknown type should have a display view");

        assert_eq!(view.source, "urn:loam:instance:instance-01");
        assert_eq!(view.message_type, "com.acme.testing.regression.detected");
        assert_eq!(view.summary, "Regression detected by the configured suite.");
        assert_eq!(view.to.len(), 1);
        assert_eq!(view.context.repository_id, "repo-2F8");
        assert_eq!(
            view.payload,
            original
                .get("data")
                .and_then(|data| data.get("payload"))
                .expect("fixture should have payload")
        );
    }

    #[test]
    fn extension_rejects_reserved_and_incompatible_schema_cases() {
        let cases = json::parse(include_str!("../tests/fixtures/mqtt/extension-cases.json"))
            .expect("extension corpus should parse");
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        for case in cases
            .as_array()
            .expect("extension corpus should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let base = case
                .get("base")
                .and_then(Value::as_str)
                .unwrap_or("extension");
            let mut message = json::parse(fixture(base)).expect("base fixture should parse");
            for patch in case
                .get("patches")
                .and_then(Value::as_array)
                .expect("case should have patches")
            {
                apply_fixture_patch(&mut message, patch);
            }
            let mut config = ValidationConfig::default();
            if name == "extension_dataschema_mismatch" {
                config.extension_schemas.push(SchemaBinding::new(
                    "com.acme.testing.regression.detected",
                    "https://schemas.acme.example/regression/1",
                ));
            }

            assert_eq!(
                validate(
                    message.to_json().as_bytes(),
                    topic_for(base),
                    &authenticated_principal(),
                    &config,
                    now,
                ),
                Err(expected_extension_violation(name)),
                "case {name}"
            );
        }
    }

    #[test]
    fn extension_additive_change_within_major_is_accepted() {
        let mut message = json::parse(fixture("extension")).expect("fixture should parse");
        apply_fixture_patch(
            &mut message,
            &json::parse(
                r#"{"op":"set","path":["data","payload","new_optional_detail"],"value":{"count":2}}"#,
            )
            .expect("patch should parse"),
        );
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);

        assert!(validate(
            message.to_json().as_bytes(),
            topic_for("extension"),
            &authenticated_principal(),
            &config,
            now,
        )
        .is_ok());
    }

    #[test]
    fn extension_validation_has_no_network_capability_for_the_fixture_corpus() {
        let config = ValidationConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        for base in ["git-refs-changed", "work-state", "message", "extension"] {
            let _ = validate_observed(
                fixture(base).as_bytes(),
                topic_for(base),
                &authenticated_principal(),
                &config,
                now,
            );
        }
        let cases = json::parse(include_str!("../tests/fixtures/mqtt/extension-cases.json"))
            .expect("extension corpus should parse");
        let remote_case = cases
            .as_array()
            .and_then(|cases| {
                cases.iter().find(|case| {
                    case.get("name").and_then(Value::as_str) == Some("unconfigured_clone_remote")
                })
            })
            .expect("clone-remote rejection case should exist");
        let mut message =
            json::parse(fixture("extension")).expect("extension fixture should parse");
        for patch in remote_case
            .get("patches")
            .and_then(Value::as_array)
            .expect("clone-remote case should have patches")
        {
            apply_fixture_patch(&mut message, patch);
        }
        assert_eq!(
            validate_observed(
                message.to_json().as_bytes(),
                topic_for("extension"),
                &authenticated_principal(),
                &config,
                now,
            ),
            Err(Violation::UnknownContextField)
        );

        // Union of the transport and connector process-capable admissions. The connector:
        // `enrollment.rs` + `service.rs` run git/manager subprocesses (the
        // isolated commit-reachability fetch and the native service managers).
        // The transport: `transport.rs` runs the git-transport subprocess. The
        // federation CLI: `federation.rs` runs git for the one-command connect
        // surface (remote-URL scope inference and the current commit). Every
        // other module stays barred; the guard is the security boundary for
        // both slices.
        let process_files = [
            "checkpoint.rs",
            "codegraph.rs",
            "enrollment.rs",
            "federation.rs",
            "main.rs",
            "provisioning.rs",
            "service.rs",
            "state.rs",
            "transport.rs",
        ];
        let filesystem_files = [
            "check.rs",
            "checkpoint.rs",
            "codegraph.rs",
            "datecheck.rs",
            "enrollment.rs",
            // The federation CLI reads the identity bundle and the registry for
            // the one-command connect surface and the lifecycle verbs.
            "federation.rs",
            // The harness read path reads the installed skill body,
            // `install.json`, and the workspace state that the retired Node
            // integration used to assemble. Reads only — it opens no file for
            // writing and the registry connection it takes is read-only.
            "harness.rs",
            "hooks.rs",
            "ipc/unix.rs",
            "markdown.rs",
            "memory.rs",
            // Reads only, and only two things: the per-project peer roster that
            // decides whom a session admits, and the identity-path PEMs.
            "provisioning.rs",
            "service.rs",
            "sha256.rs",
            "state.rs",
        ];
        // The owner-authenticated IPC endpoint uses a local Unix domain
        // socket (`UnixStream`/`UnixListener`) for same-host, same-user IPC — not
        // network egress. It is admitted here alone; every other module stays
        // barred, and no TCP/UDP/HTTP surface is ever allowed.
        let unix_socket_ipc = "ipc/unix.rs";
        // The connector's wake adapter (live-push T1): `notify-tcp://` does a
        // one-shot localhost TCP connect with a metadata-only wake frame.
        // Best-effort fire-and-forget with no persistent connection, no read
        // of any response, and errors eaten per the degrade rule; the
        // connector stays barred from subprocess spawn and from every other
        // network surface.
        let connector_wake = "connector.rs";
        // Auto-enrollment (specs/federation-auto-enrollment.md): the one
        // outbound HTTPS POST a connectionless machine makes, to the
        // broker-host signer, to obtain its client certificate. Narrow by
        // construction: a single POST with a fixed schema to a URL derived
        // from the broker host; no persistent connection, no reads of foreign
        // topics, no subprocess spawn. Every other module stays barred.
        let auto_enrollment = "enrollment_auto.rs";
        for (path, production) in crate_production_sources() {
            for forbidden in [
                "std::net",
                "TcpStream",
                "UdpSocket",
                "UnixStream",
                "reqwest",
                "ureq",
                "hyper",
                "curl ",
            ] {
                if forbidden == "UnixStream" && path == unix_socket_ipc {
                    continue;
                }
                if (forbidden == "std::net" || forbidden == "TcpStream") && path == connector_wake {
                    continue;
                }
                if (forbidden == "std::net" || forbidden == "TcpStream") && path == auto_enrollment
                {
                    continue;
                }
                assert!(
                    !production.contains(forbidden),
                    "network surface introduced in {path}: {forbidden}"
                );
            }
            // `std::process::abort` and `std::process::exit` are not subprocess
            // capabilities: each ends this process, neither starts another. The
            // Windows IPC fail-safe aborts when a cancelled overlapped operation
            // cannot be proven complete, and the connector's liveness watchdog
            // exits nonzero so its OS supervisor respawns a fresh process. Both are
            // excluded by name — every module, `ipc/windows.rs` and `connector.rs`
            // included, stays barred from `Command::new` and every other
            // subprocess-spawning `std::process` reach.
            let spawn_reach = production
                .replace("std::process::abort", "")
                .replace("std::process::exit", "");
            if spawn_reach.contains("std::process") || spawn_reach.contains("Command::new") {
                assert!(
                    process_files.contains(&path.as_str()),
                    "new process-capable module: {path}"
                );
            }
            if production.contains("std::fs")
                || production.contains("use std::fs")
                || production.contains("File::open")
            {
                assert!(
                    filesystem_files.contains(&path.as_str()),
                    "new filesystem-capable module: {path}"
                );
            }
        }

        assert_dependency_allowlist();
    }

    #[test]
    fn extension_has_no_dispatch_surface() {
        // This absence is load-bearing: the borrowed ExtensionView is data-only,
        // and callable capabilities anywhere in the crate require explicit review.
        for (path, production) in crate_production_sources() {
            assert!(
                !production.contains("dyn "),
                "trait-object capability introduced in {path}"
            );
            assert!(
                !production.contains("fn("),
                "function-pointer capability introduced in {path}"
            );
            assert!(
                !production.contains("impl ExtensionView"),
                "ExtensionView acquired behavior in {path}"
            );
        }
    }

    #[test]
    fn render_quotes_and_sanitizes_the_injection_corpus() {
        let cases = json::parse(include_str!("../tests/fixtures/mqtt/injection-corpus.json"))
            .expect("injection corpus should parse");

        for case in cases
            .as_array()
            .expect("injection corpus should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("case should have a name");
            let summary = case
                .get("summary")
                .and_then(Value::as_str)
                .expect("case should have a summary");
            let body = case.get("body").and_then(Value::as_str);
            let rendered: RenderedMessage = render_untrusted_text(summary, body);
            let mut display = String::new();
            rendered
                .write_display(&mut display)
                .expect("writing to a string should succeed");

            assert_eq!(display.matches(UNTRUSTED_BEGIN).count(), 1, "case {name}");
            assert_eq!(display.matches(UNTRUSTED_END).count(), 1, "case {name}");
            assert!(display.lines().all(|line| line.starts_with("> ")));
            assert!(!display.contains("```"), "case {name}");
            assert!(!display.contains("](http"), "case {name}");
        }
    }

    #[test]
    fn rendered_message_has_no_plain_string_conversion_or_execution_surface() {
        let source = include_str!("envelope.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("module should contain its test boundary");
        for forbidden in [
            "impl Display for RenderedMessage",
            "impl Deref for RenderedMessage",
            "impl AsRef<str> for RenderedMessage",
            "impl From<RenderedMessage> for String",
            "impl Into<String> for RenderedMessage",
        ] {
            assert!(
                !production.contains(forbidden),
                "plain-string conversion introduced: {forbidden}"
            );
        }
    }

    #[test]
    fn validate_request_and_response_thread_constructors_preserve_causality() {
        let request = Thread::for_request("thread-1", "request-1");
        assert_eq!(request.correlation_id, "request-1");
        assert_eq!(request.causation_id, None);

        let response = Thread::for_response("thread-1", "request-1", "request-1");
        assert_eq!(response.correlation_id, "request-1");
        assert_eq!(
            response.causation_id,
            Some(Value::String("request-1".to_owned()))
        );
        assert_ne!(Intent::Ack, Intent::Response);
    }

    #[test]
    #[ignore = "manual release-mode performance profile"]
    fn benchmark_full_fixture_validation_path() {
        VALIDATION_PROFILE.with(|profile| profile.set(Some((0, 0))));
        for _ in 0..100 {
            validate_rejects_structural_cases_with_specific_violations();
            validate_rejects_invalid_json_and_utf8_before_structure();
            identity_rejects_each_binding_case_with_specific_violations();
            identity_runtime_is_diagnostic_only_for_every_verdict();
            identity_subject_is_optional_and_never_defaulted();
            binding_accepts_topics_for_all_core_delivery_shapes();
            binding_rejects_each_topic_disagreement_without_normalizing();
            binding_rejects_mqtt_topic_protocol_limits();
            payload_rejects_each_schema_and_anchor_case_specifically();
            payload_accepts_every_work_state_value_and_open_message_action();
            payload_core_types_remain_validated_data_without_effects();
            extension_preserves_unknown_payload_and_exposes_display_fields();
            extension_rejects_reserved_and_incompatible_schema_cases();
            extension_additive_change_within_major_is_accepted();
        }
        let (calls, nanoseconds) = VALIDATION_PROFILE
            .with(|profile| profile.replace(None))
            .expect("benchmark should leave profiling enabled");
        let nanoseconds_per_message = nanoseconds / u128::from(calls);

        eprintln!(
            "validation profile: {calls} messages, {nanoseconds} ns total, \
             {nanoseconds_per_message} ns/message"
        );
        assert!(calls > 0);
    }

    fn fixture(name: &str) -> &'static str {
        match name {
            "git-refs-changed" => {
                include_str!("../tests/fixtures/mqtt/git-refs-changed.json")
            }
            "message" => include_str!("../tests/fixtures/mqtt/message.json"),
            "extension" => include_str!("../tests/fixtures/mqtt/extension.json"),
            _ => include_str!("../tests/fixtures/mqtt/work-state.json"),
        }
    }

    fn topic_for(name: &str) -> &'static str {
        match name {
            "git-refs-changed" => "loam/v1/org-3A1/project-7M3/event/instance-01",
            "message" => {
                "loam/v1/org-3A1/project-7M3/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC"
            }
            "extension" => "loam/v1/org-3A1/project-7M3/event/instance-01",
            _ => "loam/v1/org-3A1/project-7M3/state/instance-01/activity-01K6Q5",
        }
    }

    fn set_runtime(root: &mut Value, runtime: Option<&str>) {
        let Value::Object(outer) = root else {
            panic!("fixture root should be an object");
        };
        let data = outer
            .iter_mut()
            .find(|(name, _)| name == "data")
            .map(|(_, value)| value)
            .expect("fixture should have data");
        let Value::Object(data) = data else {
            panic!("fixture data should be an object");
        };
        let producer = data
            .iter_mut()
            .find(|(name, _)| name == "from")
            .map(|(_, value)| value)
            .expect("fixture should have from");
        let Value::Object(producer) = producer else {
            panic!("fixture from should be an object");
        };
        producer.retain(|(name, _)| name != "runtime");
        if let Some(runtime) = runtime {
            producer.push(("runtime".to_owned(), Value::String(runtime.to_owned())));
        }
    }

    fn set_display_name(root: &mut Value, name: Option<&str>) {
        set_display_name_value(root, name.map(|value| Value::String(value.to_owned())));
    }

    fn set_display_name_value(root: &mut Value, value: Option<Value>) {
        let Value::Object(outer) = root else {
            panic!("fixture root should be an object");
        };
        let data = outer
            .iter_mut()
            .find(|(name, _)| name == "data")
            .map(|(_, value)| value)
            .expect("fixture should have data");
        let Value::Object(data) = data else {
            panic!("fixture data should be an object");
        };
        let producer = data
            .iter_mut()
            .find(|(name, _)| name == "from")
            .map(|(_, value)| value)
            .expect("fixture should have from");
        let Value::Object(producer) = producer else {
            panic!("fixture from should be an object");
        };
        producer.retain(|(name, _)| name != "display_name");
        if let Some(value) = value {
            producer.push(("display_name".to_owned(), value));
        }
    }

    fn apply_fixture_patch(root: &mut Value, patch: &Value) {
        let path = patch
            .get("path")
            .and_then(Value::as_array)
            .expect("patch should have a path");
        let (field, parents) = path.split_last().expect("patch path should not be empty");
        let field = field.as_str().expect("path segment should be a string");
        let mut current = root;
        for segment in parents {
            let segment = segment.as_str().expect("path segment should be a string");
            current = match current {
                Value::Object(fields) => fields
                    .iter_mut()
                    .find(|(name, _)| name == segment)
                    .map(|(_, value)| value)
                    .expect("parent path should exist"),
                Value::Array(items) => items
                    .get_mut(
                        segment
                            .parse::<usize>()
                            .expect("array path segment should be an index"),
                    )
                    .expect("array path index should exist"),
                _ => panic!("parent path should be an object or array"),
            };
        }
        let Value::Object(fields) = current else {
            panic!("patch target should be an object");
        };
        let operation = patch
            .get("op")
            .and_then(Value::as_str)
            .expect("patch should have an operation");
        match operation {
            "remove" => {
                let index = fields
                    .iter()
                    .position(|(name, _)| name == field)
                    .expect("removed field should exist");
                fields.remove(index);
            }
            "set" => {
                let replacement = patch.get("value").expect("set should have a value").clone();
                if let Some((_, value)) = fields.iter_mut().find(|(name, _)| name == field) {
                    *value = replacement;
                } else {
                    fields.push((field.to_owned(), replacement));
                }
            }
            "duplicate" => {
                let value = fields
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, value)| value.clone())
                    .expect("duplicated field should exist");
                fields.push((field.to_owned(), value));
            }
            _ => panic!("unknown fixture patch operation"),
        }
    }

    fn expected_violation(name: &str) -> Violation {
        match name {
            "missing_specversion" => Violation::MissingSpecversion,
            "wrong_specversion" => Violation::WrongSpecversion,
            "missing_id" => Violation::MissingId,
            "missing_type" => Violation::MissingType,
            "empty_type" => Violation::MissingType,
            "missing_datacontenttype" => Violation::MissingDatacontenttype,
            "wrong_datacontenttype" => Violation::WrongDatacontenttype,
            "invalid_intent" => Violation::InvalidIntent,
            "invalid_delivery_class" => Violation::InvalidDeliveryClass,
            "missing_latest_state_key" => Violation::MissingLatestStateKey,
            "missing_latest_state_revision" => Violation::MissingLatestStateRevision,
            "duplicate_json_key" => Violation::DuplicateJsonKey,
            "duplicate_envelope_field" => Violation::DuplicateEnvelopeField,
            "oversized_payload" => Violation::PayloadTooLarge,
            "oversized_document" => Violation::DocumentTooLarge,
            "invalid_expiry" => Violation::InvalidExpiry,
            "expired" => Violation::Expired,
            "expiry_too_far" => Violation::ExpiryTooFar,
            "invalid_envelope_shape" => Violation::InvalidEnvelopeShape,
            "request_missing_thread" => Violation::MissingRequestThread,
            "request_wrong_correlation" => Violation::InvalidRequestCorrelation,
            "response_missing_thread" => Violation::MissingResponseThread,
            "response_missing_causation" => Violation::MissingResponseCausation,
            _ => panic!("unmapped rejection fixture {name}"),
        }
    }

    fn authenticated_principal() -> AuthenticatedPrincipal<'static> {
        const CLAIMS: &[&str] = &["employee-184"];
        AuthenticatedPrincipal::new("broker-user-7", CLAIMS)
    }

    fn expected_identity_violation(name: &str) -> Violation {
        match name {
            "invalid_source_scheme" => Violation::InvalidSourceScheme,
            "source_instance_mismatch" => Violation::SourceInstanceMismatch,
            "unauthorized_principal" => Violation::UnauthorizedPrincipal,
            "authentication_precedes_structure" => Violation::UnauthorizedPrincipal,
            "runtime_only_identity" => Violation::MissingStructuredIdentity,
            "payload_only_identity" => Violation::PayloadDeclaredIdentity,
            "plan_local_subject" => Violation::PlanLocalSubject,
            _ => panic!("unmapped identity fixture {name}"),
        }
    }

    fn expected_binding_violation(name: &str) -> Violation {
        match name {
            "malformed_topic" | "wildcard_topic" | "nul_topic" => Violation::MalformedTopic,
            "organization_mismatch" | "cross_organization" => {
                Violation::BindingMismatch(BindingAxis::Organization)
            }
            "project_mismatch" => Violation::BindingMismatch(BindingAxis::Project),
            "origin_mismatch" => Violation::BindingMismatch(BindingAxis::Origin),
            "delivery_class_mismatch" => Violation::BindingMismatch(BindingAxis::DeliveryClass),
            "state_key_mismatch" => Violation::BindingMismatch(BindingAxis::StateKey),
            "project_audience_mismatch"
            | "organization_audience_mismatch"
            | "direct_recipient_on_state"
            | "empty_audience" => Violation::BindingMismatch(BindingAxis::Audience),
            "inbox_recipient_mismatch" => Violation::BindingMismatch(BindingAxis::Recipient),
            "inbox_recipient_kind_mismatch" => {
                Violation::BindingMismatch(BindingAxis::RecipientKind)
            }
            "unknown_recipient_kind" => Violation::UnknownRecipientKind,
            "inbox_message_mismatch" => Violation::BindingMismatch(BindingAxis::MessageId),
            _ => panic!("unmapped binding fixture {name}"),
        }
    }

    fn expected_payload_violation(name: &str) -> Violation {
        match name {
            "missing_repository_id" => Violation::MissingRepositoryId,
            "invalid_git_refs_payload" => Violation::InvalidGitRefsPayload,
            "missing_old_oid" => Violation::MissingOldOid,
            "invalid_old_oid" => Violation::InvalidOldOid,
            "missing_new_oid" => Violation::MissingNewOid,
            "invalid_new_oid" => Violation::InvalidNewOid,
            "invalid_work_state_payload" => Violation::InvalidWorkStatePayload,
            "invalid_work_state" => Violation::InvalidWorkState,
            "missing_base_oid" => Violation::MissingBaseOid,
            "invalid_base_oid" => Violation::InvalidBaseOid,
            "missing_plan_oid" => Violation::MissingPlanOid,
            "invalid_plan_oid" => Violation::InvalidPlanOid,
            "missing_stable_artifact_id" | "missing_stable_acceptance_id" => {
                Violation::MissingStableArtifactId
            }
            "invalid_message_payload" => Violation::InvalidMessagePayload,
            "missing_message_action" => Violation::MissingMessageAction,
            "invalid_message_params" => Violation::InvalidMessageParams,
            "invalid_response_status" => Violation::InvalidResponseStatus,
            "missing_review_anchor" => Violation::MissingReviewAnchor,
            "invalid_review_commit" => Violation::InvalidReviewCommit,
            _ => panic!("unmapped payload fixture {name}"),
        }
    }

    fn expected_extension_violation(name: &str) -> Violation {
        match name {
            "reserved_type" | "spoofed_reserved_type" => Violation::ReservedType,
            "unnamespaced_type" => Violation::UnnamespacedType,
            "missing_schema_major" => Violation::MissingSchemaMajor,
            "core_dataschema_mismatch" | "extension_dataschema_mismatch" => {
                Violation::DataschemaMismatch
            }
            "unconfigured_clone_remote" | "unconfigured_git_remote" | "unknown_artifact_field" => {
                Violation::UnknownContextField
            }
            "breaking_without_new_major" => Violation::InvalidWorkState,
            _ => panic!("unmapped extension fixture {name}"),
        }
    }

    fn crate_production_sources() -> Vec<(String, String)> {
        let source_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut paths = Vec::new();
        collect_rust_sources(&source_dir, &mut paths);
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let name = path
                    .strip_prefix(&source_dir)
                    .expect("source path should remain under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                let source =
                    std::fs::read_to_string(&path).expect("Rust source should be readable");
                let production = source
                    .split("mod tests {")
                    .next()
                    .expect("split always returns one item")
                    .to_owned();
                (name, production)
            })
            .collect()
    }

    fn collect_rust_sources(directory: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("source directory should be readable") {
            let path = entry.expect("source entry should be readable").path();
            if path.is_dir() {
                collect_rust_sources(&path, output);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                output.push(path);
            }
        }
    }

    fn assert_dependency_allowlist() {
        let mut in_dependencies = false;
        for line in include_str!("../Cargo.toml").lines() {
            let line = line.trim();
            if line.starts_with('[') {
                if let Some(name) = dependency_subtable_name(line) {
                    assert_allowed_dependency(name);
                }
                in_dependencies = line.ends_with("dependencies]");
                continue;
            }
            if !in_dependencies || line.is_empty() || line.starts_with('#') {
                continue;
            }
            let name = line
                .split_once('=')
                .map(|(name, _)| name.trim().split('.').next().unwrap_or(name).trim())
                .expect("dependency should use a key-value entry");
            assert_allowed_dependency(name);
        }

        assert_eq!(
            dependency_subtable_name("[dependencies.reqwest]"),
            Some("reqwest")
        );
        assert_eq!(
            dependency_subtable_name("[target.'cfg(unix)'.dependencies.hyper]"),
            Some("hyper")
        );
    }

    fn dependency_subtable_name(header: &str) -> Option<&str> {
        let header = header.strip_prefix('[')?.strip_suffix(']')?;
        for marker in ["dependencies.", "dev-dependencies.", "build-dependencies."] {
            if let Some(name) = header.strip_prefix(marker).or_else(|| {
                header
                    .split_once(&format!(".{marker}"))
                    .map(|(_, name)| name)
            }) {
                return Some(name.trim_matches(['\'', '"']));
            }
        }
        None
    }

    fn assert_allowed_dependency(name: &str) {
        assert!(
            [
                "chrono",
                "pulldown-cmark",
                "ring",
                "rumqttc",
                "rusqlite",
                // Auto-enrollment's HTTPS client and keypair generation: already
                // compiled in the tree as transitive rustls/rumqttc providers,
                // promoted to direct for the one outbound POST (see
                // enrollment_auto.rs). `rustls` is the TLS stack; `ring` is the
                // deterministic provider for TLS and ECDSA P-256 keygen/signing.
                "rustls",
                "rustls-pemfile",
                // Bundled Mozilla trust roots: the no-`ca_ref` default trust
                // path, replacing the per-OS trust-file search. Compiled-in
                // data, no network code.
                "webpki-roots",
            ]
            .contains(&name),
            "network-capable dependency requires review: {name}"
        );
    }
}
