package com.hawkeynl.jarvis.network

import java.io.File

interface JarvisApi {
    suspend fun ready(endpoint: HomeNodeEndpoint): ApiResult<HealthResponse>
    suspend fun createPairing(
        endpoint: HomeNodeEndpoint,
        request: PairingCreateRequest,
    ): ApiResult<PairingTicket>

    suspend fun pairingStatus(
        endpoint: HomeNodeEndpoint,
        ticket: PairingTicket,
    ): ApiResult<PairingStatusResponse>

    suspend fun challenge(
        endpoint: HomeNodeEndpoint,
        request: ChallengeRequest,
    ): ApiResult<ChallengeResponse>

    suspend fun login(
        endpoint: HomeNodeEndpoint,
        request: LoginRequest,
    ): ApiResult<LoginResponse>

    suspend fun logout(endpoint: HomeNodeEndpoint, token: String): ApiResult<Unit>
    suspend fun deleteDevice(
        endpoint: HomeNodeEndpoint,
        token: String,
        deviceId: String,
    ): ApiResult<Unit>

    suspend fun conversations(
        endpoint: HomeNodeEndpoint,
        token: String,
    ): ApiResult<ConversationListResponse>

    suspend fun conversation(
        endpoint: HomeNodeEndpoint,
        token: String,
        id: String,
    ): ApiResult<ConversationDetailResponse>

    suspend fun chat(
        endpoint: HomeNodeEndpoint,
        token: String,
        request: ChatRequest,
    ): ApiResult<ChatResponse>

    suspend fun androidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        currentVersionCode: Int,
        clientProtocol: Int,
    ): ApiResult<AndroidUpdateMetadata?>

    suspend fun downloadAndroidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        destination: File,
        expectedSize: Long,
    ): ApiResult<Unit>
}
