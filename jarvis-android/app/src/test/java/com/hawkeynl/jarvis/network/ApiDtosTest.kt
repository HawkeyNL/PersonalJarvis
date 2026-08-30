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

    @Test
    fun `Android update metadata decodes version signer hash and protocol`() {
        val decoded = json.decodeFromString(
            AndroidUpdateMetadata.serializer(),
            """{"schema_version":1,"platform":"android","package_name":"com.hawkeynl.jarvis","version_code":42,"version_name":"1.2.3","minimum_client_protocol":1,"released_at":"2026-08-30T12:00:00Z","notes":"Update","artifact":{"size":2048,"sha256":"${"a".repeat(64)}","signing_certificate_sha256":"${"b".repeat(64)}"},"download_url":"https://jarvis.example/v1/app-updates/android/download"}""",
        )

        assertEquals(42, decoded.version_code)
        assertEquals(1, decoded.minimum_client_protocol)
        assertEquals("b".repeat(64), decoded.artifact.signing_certificate_sha256)
    }

    private fun assertNull(value: Any?) = org.junit.Assert.assertNull(value)
}
