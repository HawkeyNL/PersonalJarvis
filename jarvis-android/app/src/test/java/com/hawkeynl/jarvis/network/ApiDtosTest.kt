package com.hawkeynl.jarvis.network

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ApiDtosTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    @Test
    fun `pairing request matches deny-unknown-fields server contract`() {
        val encoded = json.encodeToString(
            PairingCreateRequest.serializer(),
            PairingCreateRequest(
                name = "Jarvis Android",
                platform = "android",
                public_key = "ab".repeat(32),
            ),
        )

        assertTrue(encoded.contains("\"platform\":\"android\""))
        assertTrue(encoded.contains("\"public_key\":"))
        assertFalse(encoded.contains("device_id"))
    }

    @Test
    fun `chat refusal response decodes despite omitted optional routing fields`() {
        val decoded = json.decodeFromString(
            ChatResponse.serializer(),
            """{"reply":"Sorry","model":null,"stop_reason":"refusal","conversation_id":"c","conversation_title":"Nieuw","new_topic":true}""",
        )

        assertEquals("c", decoded.conversation_id)
        assertNull(decoded.backend)
    }

    private fun assertNull(value: Any?) = org.junit.Assert.assertNull(value)
}
