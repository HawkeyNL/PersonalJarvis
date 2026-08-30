package com.hawkeynl.jarvis.security

interface DeviceIdentity {
    fun publicKeyHex(): String
    fun signHex(messageHex: String): String
    fun reset()
}
