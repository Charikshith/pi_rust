//! The `pi-ai` type model — Rust port of the data types in `packages/ai/src/types.ts`.
//!
//! Modules mirror logical groups of the source file. Function/contract interfaces
//! (`StreamOptions`, `ProviderStreams`, `StreamFunction`), the images types, and the
//! typed provider `compat` unions are intentionally deferred to the runtime/provider
//! phases (feat-002 / feat-008); this module is the serializable data vocabulary that
//! every other crate shares.

pub mod content;
pub mod event;
pub mod ids;
pub mod message;
pub mod model;
pub mod usage;

pub use content::{
    AssistantContent, ImageContent, ImageTag, TextContent, TextTag, ThinkingContent, ThinkingTag,
    ToolCall, ToolCallTag, UserContent,
};
pub use event::AssistantMessageEvent;
pub use ids::{
    Api, CacheRetention, ChatTemplateKwargValue, ChatTemplateVar, ChatTemplateVarName, ImagesApi,
    ModelThinkingLevel, ProviderId, SessionAffinityFormat, StopReason, TextSignaturePhase,
    TextSignatureV1, ThinkingBudgets, ThinkingLevel, Transport,
};
pub use message::{
    AssistantMessage, AssistantRole, Context, Message, Tool, ToolResultMessage, ToolResultRole,
    UserMessage, UserMessageContent, UserRole,
};
pub use model::{Modality, Model, ModelCost, ModelCostRates, ModelCostTier, ThinkingLevelMap};
pub use usage::{Cost, Usage};
