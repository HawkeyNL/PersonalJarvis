package com.hawkeynl.jarvis.network

import kotlinx.serialization.Serializable

@Serializable
data class HealthResponse(val status: String)

@Serializable
data class ErrorResponse(
    val error: String? = null,
    val hint: String? = null,
    val conversation_id: String? = null,
)

@Serializable
data class PairingCreateRequest(
    val name: String,
    val platform: String,
    val public_key: String,
)

@Serializable
data class PairingTicket(
    val request_id: String,
    val nonce: String,
    val expires_at: Long,
)

@Serializable
data class PairingStatusResponse(
    val status: String,
    val device_id: String? = null,
)

@Serializable
data class ChallengeRequest(val device_id: String)

@Serializable
data class ChallengeResponse(
    val challenge_id: String,
    val nonce: String,
)

@Serializable
data class LoginRequest(
    val device_id: String,
    val challenge_id: String,
    val signature: String,
)

@Serializable
data class LoginResponse(
    val token: String,
    val expires_at: Long,
)

@Serializable
data class ConversationSummary(
    val id: String,
    val title: String,
    val updated_at: String,
)

@Serializable
data class ConversationListResponse(val conversations: List<ConversationSummary>)

@Serializable
data class ConversationMessage(
    val role: String,
    val content: String,
    val model: String? = null,
    val at: String,
)

@Serializable
data class ConversationDetailResponse(
    val id: String,
    val title: String,
    val messages: List<ConversationMessage>,
)

@Serializable
data class ChatTurn(
    val role: String,
    val content: String,
)

@Serializable
data class ChatRequest(
    val messages: List<ChatTurn>,
    val conversation_id: String? = null,
)

@Serializable
data class ChatResponse(
    val reply: String,
    val model: String? = null,
    val backend: String? = null,
    val routing_reason: String? = null,
    val stop_reason: String? = null,
    val conversation_id: String,
    val conversation_title: String,
    val new_topic: Boolean,
    val routing_mode: String? = null,
)

@Serializable
data class AndroidUpdateArtifact(
    val size: Long,
    val sha256: String,
    val signing_certificate_sha256: String,
)

@Serializable
data class AndroidUpdateMetadata(
    val schema_version: Int,
    val platform: String,
    val package_name: String,
    val version_code: Int,
    val version_name: String,
    val minimum_client_protocol: Int,
    val released_at: String,
    val notes: String,
    val artifact: AndroidUpdateArtifact,
    val download_url: String,
)
