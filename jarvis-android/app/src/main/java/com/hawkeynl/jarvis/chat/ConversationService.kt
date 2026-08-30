package com.hawkeynl.jarvis.chat

import com.hawkeynl.jarvis.network.ApiResult
import com.hawkeynl.jarvis.network.ChatRequest
import com.hawkeynl.jarvis.network.ChatResponse
import com.hawkeynl.jarvis.network.ChatTurn
import com.hawkeynl.jarvis.network.ConversationDetailResponse
import com.hawkeynl.jarvis.network.ConversationListResponse
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import com.hawkeynl.jarvis.network.JarvisApi
import com.hawkeynl.jarvis.storage.SessionRepository

class ConversationService(
    private val api: JarvisApi,
    private val sessions: SessionRepository,
) {
    suspend fun list(endpoint: HomeNodeEndpoint): ApiResult<ConversationListResponse> =
        withToken { api.conversations(endpoint, it) }

    suspend fun load(endpoint: HomeNodeEndpoint, id: String): ApiResult<ConversationDetailResponse> =
        withToken { api.conversation(endpoint, it, id) }

    suspend fun send(
        endpoint: HomeNodeEndpoint,
        conversationId: String?,
        history: List<ChatTurn>,
        text: String,
    ): ApiResult<ChatResponse> {
        val message = text.trim()
        if (message.isEmpty()) return ApiResult.InvalidResponse("Bericht is leeg.")
        val turns = (history + ChatTurn("user", message)).takeLast(MAX_TURNS)
        return withToken { token -> api.chat(endpoint, token, ChatRequest(turns, conversationId)) }
    }

    private suspend fun <T> withToken(block: suspend (String) -> ApiResult<T>): ApiResult<T> {
        val token = sessions.session().token ?: return ApiResult.Unauthorized
        val result = block(token)
        if (result == ApiResult.Unauthorized) sessions.clearToken()
        return result
    }

    private companion object {
        const val MAX_TURNS = 20
    }
}
