package com.hawkeynl.jarvis.auth

import com.hawkeynl.jarvis.network.AndroidUpdateMetadata
import com.hawkeynl.jarvis.network.ApiResult
import com.hawkeynl.jarvis.network.ChallengeRequest
import com.hawkeynl.jarvis.network.ChallengeResponse
import com.hawkeynl.jarvis.network.ChatRequest
import com.hawkeynl.jarvis.network.ChatResponse
import com.hawkeynl.jarvis.network.ConversationDetailResponse
import com.hawkeynl.jarvis.network.ConversationListResponse
import com.hawkeynl.jarvis.network.EndpointValidation
import com.hawkeynl.jarvis.network.HealthResponse
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import com.hawkeynl.jarvis.network.JarvisApi
import com.hawkeynl.jarvis.network.LoginRequest
import com.hawkeynl.jarvis.network.LoginResponse
import com.hawkeynl.jarvis.network.PairingCreateRequest
import com.hawkeynl.jarvis.network.PairingStatusResponse
import com.hawkeynl.jarvis.network.PairingTicket
import com.hawkeynl.jarvis.security.DeviceIdentity
import com.hawkeynl.jarvis.storage.SessionRepository
import com.hawkeynl.jarvis.testing.InMemorySecureValueStore
import java.io.File
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollmentServiceTest {
    private val endpoint = (HomeNodeEndpoint.parse("https://jarvis.test") as EndpointValidation.Valid).endpoint

    @Test
    fun `persists pending request then signs challenge after approval`() = runTest {
        val api = FakeApi()
        val sessions = SessionRepository(InMemorySecureValueStore())
        val service = EnrollmentService(api, FakeIdentity(), sessions)

        val pending = service.startOrResume(endpoint, "Android", 100)
        assertTrue(pending is EnrollmentOutcome.Pending)
        assertEquals("android", api.created?.platform)
        assertEquals("ticket", sessions.pairingTicket()?.request_id)

        api.approved = true
        val authenticated = service.poll(endpoint)
        assertTrue(authenticated is EnrollmentOutcome.Authenticated)
        assertEquals("device", sessions.session().deviceId)
        assertEquals("token", sessions.session().token)
        assertEquals("cd".repeat(64), api.loginRequest?.signature)
    }
}

private class FakeIdentity : DeviceIdentity {
    override fun publicKeyHex() = "ab".repeat(32)
    override fun signHex(messageHex: String) = "cd".repeat(64)
    override fun reset() = Unit
}

private class FakeApi : JarvisApi {
    var created: PairingCreateRequest? = null
    var approved = false
    var loginRequest: LoginRequest? = null

    override suspend fun ready(endpoint: HomeNodeEndpoint) = ApiResult.Success(HealthResponse("ready"))
    override suspend fun createPairing(endpoint: HomeNodeEndpoint, request: PairingCreateRequest): ApiResult<PairingTicket> {
        created = request
        return ApiResult.Success(PairingTicket("ticket", "01".repeat(32), 500))
    }
    override suspend fun pairingStatus(endpoint: HomeNodeEndpoint, ticket: PairingTicket) =
        ApiResult.Success(PairingStatusResponse(if (approved) "approved" else "pending", if (approved) "device" else null))
    override suspend fun challenge(endpoint: HomeNodeEndpoint, request: ChallengeRequest) =
        ApiResult.Success(ChallengeResponse("challenge", "02".repeat(32)))
    override suspend fun login(endpoint: HomeNodeEndpoint, request: LoginRequest): ApiResult<LoginResponse> {
        loginRequest = request
        return ApiResult.Success(LoginResponse("token", 900))
    }
    override suspend fun logout(endpoint: HomeNodeEndpoint, token: String) = ApiResult.Success(Unit)
    override suspend fun deleteDevice(endpoint: HomeNodeEndpoint, token: String, deviceId: String) = ApiResult.Success(Unit)
    override suspend fun conversations(endpoint: HomeNodeEndpoint, token: String): ApiResult<ConversationListResponse> = error("unused")
    override suspend fun conversation(endpoint: HomeNodeEndpoint, token: String, id: String): ApiResult<ConversationDetailResponse> = error("unused")
    override suspend fun chat(endpoint: HomeNodeEndpoint, token: String, request: ChatRequest): ApiResult<ChatResponse> = error("unused")
    override suspend fun androidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        currentVersionCode: Int,
        clientProtocol: Int,
    ): ApiResult<AndroidUpdateMetadata?> = error("unused")
    override suspend fun downloadAndroidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        destination: File,
        expectedSize: Long,
    ): ApiResult<Unit> = error("unused")
}
