//! Why the direct transport refused an envelope, as data: the log names the check that failed and
//! the sender only ever learns more than «unauthorized» once its own signature proved who it is.

/// Why the direct transport refused an envelope; it maps to the HTTP status.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirectRefusal {
    /// Addressed to another Mac, or not a request.
    Misdirected,
    /// Not from a device approved on this Mac, badly signed, or expired without proving its sender.
    Unauthorized(UnauthorizedCheck),
    /// Signed by the approved device, but it reached the Mac after it expired: the sealed answer
    /// carries the Mac's clock so the device can seal again.
    Expired { late_ms: u64, sealed: Vec<u8> },
    /// The Mac could not read its own approvals right now: nothing was decided about the device.
    Unavailable,
    /// The request ID was used before with other bytes.
    Conflict,
    /// From an approved device, but not something the service runs.
    Rejected(String),
    /// From a device revoked on this Mac and signed by it: the sealed answer saying so.
    Revoked(Vec<u8>),
}

/// The check an unauthorized envelope failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnauthorizedCheck {
    UnknownDevice,
    NotApproved,
    WrongIdentity,
    Expired { late_ms: u64 },
    BadEncoding,
    BadSignature,
    ApprovalChanged,
}

impl UnauthorizedCheck {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::UnknownDevice => "unknown_device",
            Self::NotApproved => "not_approved",
            Self::WrongIdentity => "wrong_identity",
            Self::Expired { .. } => "expired",
            Self::BadEncoding => "bad_encoding",
            Self::BadSignature => "bad_signature",
            Self::ApprovalChanged => "approval_changed",
        }
    }

    pub(crate) fn of(error: &alondra_e2e::Error, expires_at_ms: u64, now_ms: u64) -> Self {
        use alondra_e2e::Error;
        match error {
            Error::WrongIdentity => Self::WrongIdentity,
            Error::Expired => Self::Expired { late_ms: now_ms.saturating_sub(expires_at_ms) },
            Error::InvalidSignature | Error::InvalidKey | Error::Crypto => Self::BadSignature,
            _ => Self::BadEncoding,
        }
    }
}

impl DirectRefusal {
    /// The fields the log names for this refusal, never a signature, key or content.
    pub(crate) fn log_fields(&self) -> String {
        match self {
            Self::Misdirected => "status=421 reason=wrong_mac".into(),
            Self::Unauthorized(check @ UnauthorizedCheck::Expired { late_ms }) => {
                format!("status=401 reason=unauthorized_envelope check={} late_ms={late_ms}", check.name())
            }
            Self::Unauthorized(check) => format!("status=401 reason=unauthorized_envelope check={}", check.name()),
            Self::Expired { late_ms, .. } => format!("status=401 reason=envelope_expired check=expired late_ms={late_ms}"),
            Self::Unavailable => "status=503 reason=approvals_unreadable".into(),
            Self::Conflict => "status=409 reason=idempotency_conflict".into(),
            Self::Rejected(_) => "status=400 reason=request_rejected".into(),
            Self::Revoked(_) => "status=403 reason=device_revoked".into(),
        }
    }
}

/// The Mac could not read `devices.json`; the request is refused without judging the device.
#[derive(Debug)]
pub(crate) struct ApprovalsUnreadable(pub(crate) String);

impl std::fmt::Display for ApprovalsUnreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the approved devices could not be read: {}", self.0)
    }
}

impl std::error::Error for ApprovalsUnreadable {}
