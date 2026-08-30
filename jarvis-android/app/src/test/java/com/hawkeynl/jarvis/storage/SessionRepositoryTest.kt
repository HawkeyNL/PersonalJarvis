package com.hawkeynl.jarvis.storage

import com.hawkeynl.jarvis.network.PairingTicket
import com.hawkeynl.jarvis.testing.InMemorySecureValueStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SessionRepositoryTest {
    @Test
    fun `session and pairing state survive repository recreation and reset together`() {
        val store = InMemorySecureValueStore()
        val first = SessionRepository(store)
        first.savePairingTicket(PairingTicket("request", "a".repeat(64), 1234))
        first.saveLogin("device", "secret-token", 5678)

        val restored = SessionRepository(store)
        assertTrue(restored.hasSessionRecord())
        assertEquals("device", restored.session().deviceId)
        assertEquals("secret-token", restored.session().token)
        assertEquals("request", restored.pairingTicket()?.request_id)

        restored.clearToken()
        assertNull(restored.session().token)
        restored.reset()
        assertFalse(restored.hasSessionRecord())
        assertNull(restored.pairingTicket())
    }
}
