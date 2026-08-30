package com.hawkeynl.jarvis.security

object Hex {
    fun encode(bytes: ByteArray): String = buildString(bytes.size * 2) {
        bytes.forEach { byte -> append("%02x".format(byte.toInt() and 0xff)) }
    }

    fun decode(value: String): ByteArray {
        require(value.length % 2 == 0) { "Hex value must have an even length" }
        return ByteArray(value.length / 2) { index ->
            val high = Character.digit(value[index * 2], 16)
            val low = Character.digit(value[index * 2 + 1], 16)
            require(high >= 0 && low >= 0) { "Invalid hex value" }
            ((high shl 4) or low).toByte()
        }
    }
}
