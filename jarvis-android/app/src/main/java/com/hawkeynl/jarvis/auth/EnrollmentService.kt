package com.hawkeynl.jarvis.auth

import com.hawkeynl.jarvis.network.ApiResult
import com.hawkeynl.jarvis.network.ChallengeRequest
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import com.hawkeynl.jarvis.network.JarvisApi
import com.hawkeynl.jarvis.network.LoginRequest
import com.hawkeynl.jarvis.network.PairingCreateRequest
import com.hawkeynl.jarvis.network.PairingTicket
import com.hawkeynl.jarvis.network.UnreachableReason
import com.hawkeynl.jarvis.security.DeviceIdentity
import com.hawkeynl.jarvis.storage.ProtectedSession
import com.hawkeynl.jarvis.storage.SessionRepository

sealed interface EnrollmentOutcome {
    data class Pending(val ticket: PairingTicket) : EnrollmentOutcome
    data class Authenticated(val expiresAt: Long) : EnrollmentOutcome
    data object Denied : EnrollmentOutcome
    data object Expired : EnrollmentOutcome
    data object Unauthorized : EnrollmentOutcome
    data class Unreachable(val reason: UnreachableReason) : EnrollmentOutcome
    data class Rejected(val status: Int, val message: String?) : EnrollmentOutcome
    data class InvalidResponse(val message: String) : EnrollmentOutcome
}

class EnrollmentService(
    private val api: JarvisApi,
    private val identity: DeviceIdentity,
    private val sessions: SessionRepository,
) {
    fun currentSession(): ProtectedSession = sessions.session()

    suspend fun startOrResume(
        endpoint: HomeNodeEndpoint,
        deviceName: String,
        nowEpochSeconds: Long,
    ): EnrollmentOutcome {
        sessions.session().deviceId?.let { return login(endpoint, it) }

        val existing = sessions.pairingTicket()
        if (existing != null) {
            if (existing.expires_at <= nowEpochSeconds) {
                sessions.clearPairingTicket()
                return EnrollmentOutcome.Expired
            }
            return poll(endpoint, existing)
        }

        return when (val result = api.createPairing(
            endpoint,
            PairingCreateRequest(
                name = deviceName.take(80),
                platform = "android",
                public_key = identity.publicKeyHex(),
            ),
        )) {
            is ApiResult.Success -> {
                sessions.savePairingTicket(result.value)
                EnrollmentOutcome.Pending(result.value)
            }
            else -> result.toEnrollmentFailure()
        }
    }

    suspend fun poll(endpoint: HomeNodeEndpoint): EnrollmentOutcome {
        val ticket = sessions.pairingTicket()
            ?: return EnrollmentOutcome.InvalidResponse("Geen openstaand koppelverzoek.")
        return poll(endpoint, ticket)
    }

    suspend fun login(endpoint: HomeNodeEndpoint): EnrollmentOutcome {
        val deviceId = sessions.session().deviceId ?: return EnrollmentOutcome.Unauthorized
        return login(endpoint, deviceId)
    }

    suspend fun logout(endpoint: HomeNodeEndpoint) {
        val token = sessions.session().token
        if (token != null) api.logout(endpoint, token)
        sessions.clearToken()
    }

    suspend fun resetDevice(endpoint: HomeNodeEndpoint?) {
        val session = sessions.session()
        if (endpoint != null && session.token != null && session.deviceId != null) {
            api.deleteDevice(endpoint, session.token, session.deviceId)
        }
        sessions.reset()
        identity.reset()
    }

    private suspend fun poll(endpoint: HomeNodeEndpoint, ticket: PairingTicket): EnrollmentOutcome {
        return when (val result = api.pairingStatus(endpoint, ticket)) {
            is ApiResult.Success -> when (result.value.status) {
                "pending" -> EnrollmentOutcome.Pending(ticket)
                "approved" -> {
                    val deviceId = result.value.device_id
                        ?: return EnrollmentOutcome.InvalidResponse("Goedgekeurd verzoek mist device_id.")
                    sessions.saveDeviceId(deviceId)
                    sessions.clearPairingTicket()
                    login(endpoint, deviceId)
                }
                "denied" -> {
                    sessions.clearPairingTicket()
                    EnrollmentOutcome.Denied
                }
                "expired" -> {
                    sessions.clearPairingTicket()
                    EnrollmentOutcome.Expired
                }
                else -> EnrollmentOutcome.InvalidResponse("Onbekende koppelstatus.")
            }
            else -> result.toEnrollmentFailure()
        }
    }

    private suspend fun login(endpoint: HomeNodeEndpoint, deviceId: String): EnrollmentOutcome {
        val challenge = when (val result = api.challenge(endpoint, ChallengeRequest(deviceId))) {
            is ApiResult.Success -> result.value
            else -> return result.toEnrollmentFailure()
        }
        if (challenge.nonce.length != 64) {
            return EnrollmentOutcome.InvalidResponse("Loginchallenge heeft een ongeldige nonce.")
        }
        val signature = runCatching { identity.signHex(challenge.nonce) }
            .getOrElse { return EnrollmentOutcome.InvalidResponse("Device-identiteit kan niet ondertekenen.") }
        return when (val result = api.login(
            endpoint,
            LoginRequest(deviceId, challenge.challenge_id, signature),
        )) {
            is ApiResult.Success -> {
                sessions.saveLogin(deviceId, result.value.token, result.value.expires_at)
                EnrollmentOutcome.Authenticated(result.value.expires_at)
            }
            else -> result.toEnrollmentFailure()
        }
    }
}

private fun ApiResult<*>.toEnrollmentFailure(): EnrollmentOutcome = when (this) {
    ApiResult.Unauthorized -> EnrollmentOutcome.Unauthorized
    is ApiResult.Unreachable -> EnrollmentOutcome.Unreachable(reason)
    is ApiResult.HttpError -> EnrollmentOutcome.Rejected(status, message)
    is ApiResult.InvalidResponse -> EnrollmentOutcome.InvalidResponse(message)
    is ApiResult.Success -> error("Success is not a failure")
}
