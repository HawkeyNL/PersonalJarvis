//! Policy primitives for Jarvis capabilities.
//!
//! This crate intentionally contains no execution code. It defines the typed
//! vocabulary that the future Tool Registry and Policy Engine can use to decide
//! whether an agent may perform an operation. Execution remains outside this
//! crate so policy can stay deterministic and easy to test.

use serde::{Deserialize, Serialize};

/// Coarse capability classes exposed to agents/tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ReadSystem,
    ReadData,
    WriteData,
    ExecuteCode,
    ManageServices,
    ManageFiles,
    TradingRead,
    TradingPropose,
    TradingExecute,
    Admin,
}

/// Risk attached to a requested capability invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    ReadOnly,
    Reversible,
    Mutating,
    Destructive,
    Financial,
    Administrative,
}

/// The identity and context that a policy decision must consider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyContext {
    pub capability: Capability,
    pub risk: RiskClass,
    /// Whether the caller is a trusted Jarvis device/session.
    pub trusted_device: bool,
    /// Whether the operation is explicitly approved by the user/device.
    pub approved: bool,
    /// Whether the operation is reversible without data loss or external side effects.
    pub reversible: bool,
}

/// Deterministic policy outcome. The policy layer does not execute anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    RequireApproval,
    Deny,
}

/// Conservative baseline policy for capability execution.
///
/// This is deliberately small. Domain-specific rules (especially live trading)
/// should be layered on top rather than hidden inside individual tools.
pub fn decide(context: &PolicyContext) -> PolicyDecision {
    if !context.trusted_device {
        return PolicyDecision::Deny;
    }

    if context.risk >= RiskClass::Financial || context.risk >= RiskClass::Administrative {
        return if context.approved {
            PolicyDecision::Allow
        } else {
            PolicyDecision::RequireApproval
        };
    }

    match context.capability {
        Capability::TradingExecute | Capability::Admin => {
            if context.approved {
                PolicyDecision::Allow
            } else {
                PolicyDecision::RequireApproval
            }
        }
        Capability::ExecuteCode | Capability::ManageServices => {
            if context.approved {
                PolicyDecision::Allow
            } else {
                PolicyDecision::RequireApproval
            }
        }
        Capability::ManageFiles | Capability::WriteData => {
            if context.reversible || context.approved {
                PolicyDecision::Allow
            } else {
                PolicyDecision::RequireApproval
            }
        }
        Capability::ReadSystem | Capability::ReadData | Capability::TradingRead => {
            PolicyDecision::Allow
        }
        Capability::TradingPropose => PolicyDecision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(capability: Capability, risk: RiskClass) -> PolicyContext {
        PolicyContext {
            capability,
            risk,
            trusted_device: true,
            approved: false,
            reversible: false,
        }
    }

    #[test]
    fn untrusted_call_is_always_denied() {
        let mut ctx = context(Capability::ReadSystem, RiskClass::ReadOnly);
        ctx.trusted_device = false;
        assert_eq!(decide(&ctx), PolicyDecision::Deny);
    }

    #[test]
    fn read_only_capability_is_allowed() {
        let ctx = context(Capability::ReadSystem, RiskClass::ReadOnly);
        assert_eq!(decide(&ctx), PolicyDecision::Allow);
    }

    #[test]
    fn code_execution_requires_approval() {
        let ctx = context(Capability::ExecuteCode, RiskClass::Mutating);
        assert_eq!(decide(&ctx), PolicyDecision::RequireApproval);
    }

    #[test]
    fn approved_code_execution_is_allowed() {
        let mut ctx = context(Capability::ExecuteCode, RiskClass::Mutating);
        ctx.approved = true;
        assert_eq!(decide(&ctx), PolicyDecision::Allow);
    }

    #[test]
    fn live_trading_requires_approval() {
        let ctx = context(Capability::TradingExecute, RiskClass::Financial);
        assert_eq!(decide(&ctx), PolicyDecision::RequireApproval);
    }

    #[test]
    fn reversible_file_change_can_be_automatic() {
        let mut ctx = context(Capability::ManageFiles, RiskClass::Mutating);
        ctx.reversible = true;
        assert_eq!(decide(&ctx), PolicyDecision::Allow);
    }
}
