package com.hawkeynl.jarvis.network

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class HomeNodeEndpointTest {
    @Test
    fun `accepts secure remote and local cleartext endpoints`() {
        val secure = HomeNodeEndpoint.parse("https://jarvis.example.test/")
        val local = HomeNodeEndpoint.parse("http://192.168.1.24:8080", allowInsecureLocal = true)

        assertEquals("https://jarvis.example.test", (secure as EndpointValidation.Valid).endpoint.baseUrl)
        assertEquals("http://192.168.1.24:8080", (local as EndpointValidation.Valid).endpoint.baseUrl)
    }

    @Test
    fun `release policy rejects cleartext even for local hosts`() {
        assertTrue(
            HomeNodeEndpoint.parse("http://192.168.1.24:8080", allowInsecureLocal = false) is EndpointValidation.Invalid,
        )
        assertTrue(
            HomeNodeEndpoint.parse("http://localhost:8080", allowInsecureLocal = false) is EndpointValidation.Invalid,
        )
    }

    @Test
    fun `rejects public cleartext and credential-bearing URLs`() {
        assertTrue(HomeNodeEndpoint.parse("http://example.test") is EndpointValidation.Invalid)
        assertTrue(HomeNodeEndpoint.parse("https://token@example.test") is EndpointValidation.Invalid)
        assertTrue(HomeNodeEndpoint.parse("https://example.test/internal") is EndpointValidation.Invalid)
    }
}
