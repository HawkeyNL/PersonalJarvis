package com.hawkeynl.jarvis.network

import com.hawkeynl.jarvis.BuildConfig
import java.net.URI

@JvmInline
value class HomeNodeEndpoint private constructor(val baseUrl: String) {
    companion object {
        fun parse(raw: String, allowInsecureLocal: Boolean = BuildConfig.DEBUG): EndpointValidation {
            val input = raw.trim().trimEnd('/')
            if (input.isEmpty()) return EndpointValidation.Invalid("Voer het adres van je Home Node in.")

            val uri = runCatching { URI(input) }.getOrNull()
                ?: return EndpointValidation.Invalid("Dit is geen geldig webadres.")
            val scheme = uri.scheme?.lowercase()
            if (scheme != "https" && scheme != "http") {
                return EndpointValidation.Invalid("Gebruik https://, of http:// voor een lokaal netwerk.")
            }
            val host = uri.host
                ?: return EndpointValidation.Invalid("Het Home Node-adres mist een hostnaam.")
            if (host.isBlank() || uri.userInfo != null || uri.query != null || uri.fragment != null) {
                return EndpointValidation.Invalid("Gebruik alleen een Home Node-adres, zonder login, query of fragment.")
            }
            if (uri.path.orEmpty().let { it.isNotEmpty() && it != "/" }) {
                return EndpointValidation.Invalid("Het Home Node-adres mag geen pad bevatten.")
            }
            if (scheme == "http" && (!allowInsecureLocal || !isLocalHost(host))) {
                return EndpointValidation.Invalid("HTTP is alleen toegestaan voor lokale Home Nodes in een debug-build.")
            }
            val displayHost = if (host.contains(':')) "[$host]" else host
            val authority = if (uri.port == -1) displayHost else "$displayHost:${uri.port}"
            return EndpointValidation.Valid(HomeNodeEndpoint("$scheme://$authority"))
        }

        private fun isLocalHost(host: String): Boolean {
            val normalized = host.lowercase().removePrefix("[").removeSuffix("]")
            if (normalized == "localhost" || normalized == "::1" || normalized.endsWith(".local")) return true
            if (normalized.startsWith("fc") || normalized.startsWith("fd") || normalized.startsWith("fe80:")) return true
            val octets = normalized.split('.').mapNotNull(String::toIntOrNull)
            if (octets.size != 4 || octets.any { it !in 0..255 }) return false
            return octets[0] == 10 ||
                octets[0] == 127 ||
                (octets[0] == 192 && octets[1] == 168) ||
                (octets[0] == 172 && octets[1] in 16..31)
        }
    }
}

sealed interface EndpointValidation {
    data class Valid(val endpoint: HomeNodeEndpoint) : EndpointValidation
    data class Invalid(val message: String) : EndpointValidation
}
