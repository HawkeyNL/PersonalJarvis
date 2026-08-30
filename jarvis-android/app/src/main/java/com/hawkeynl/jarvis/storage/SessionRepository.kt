package com.hawkeynl.jarvis.storage

import com.hawkeynl.jarvis.network.PairingTicket
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class ProtectedSession(
    val deviceId: String? = null,
    val token: String? = null,
    val expiresAt: Long? = null,
)

class SessionRepository(
    private val secureStore: SecureValueStore,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    fun hasSessionRecord(): Boolean = secureStore.exists(SESSION)

    fun session(): ProtectedSession = secureStore.read(SESSION)?.let {
        json.decodeFromString<ProtectedSession>(it.decodeToString())
    } ?: ProtectedSession()

    fun saveDeviceId(deviceId: String) {
        saveSession(session().copy(deviceId = deviceId, token = null, expiresAt = null))
    }

    fun saveLogin(deviceId: String, token: String, expiresAt: Long) {
        saveSession(ProtectedSession(deviceId, token, expiresAt))
    }

    fun clearToken() {
        val current = session()
        saveSession(current.copy(token = null, expiresAt = null))
    }

    fun pairingTicket(): PairingTicket? = secureStore.read(PAIRING)?.let {
        json.decodeFromString<PairingTicket>(it.decodeToString())
    }

    fun savePairingTicket(ticket: PairingTicket) {
        secureStore.write(PAIRING, json.encodeToString(PairingTicket.serializer(), ticket).encodeToByteArray())
    }

    fun clearPairingTicket() = secureStore.remove(PAIRING)

    fun reset() {
        secureStore.remove(PAIRING)
        secureStore.remove(SESSION)
    }

    private fun saveSession(session: ProtectedSession) {
        secureStore.write(SESSION, json.encodeToString(ProtectedSession.serializer(), session).encodeToByteArray())
    }

    private companion object {
        const val SESSION = "session.v1"
        const val PAIRING = "pairing.v1"
    }
}
