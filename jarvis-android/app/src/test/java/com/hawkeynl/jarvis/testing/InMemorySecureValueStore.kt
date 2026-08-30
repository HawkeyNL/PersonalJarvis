package com.hawkeynl.jarvis.testing

import com.hawkeynl.jarvis.storage.SecureValueStore

class InMemorySecureValueStore : SecureValueStore {
    private val values = mutableMapOf<String, ByteArray>()

    override fun exists(key: String): Boolean = values.containsKey(key)
    override fun read(key: String): ByteArray? = values[key]?.copyOf()
    override fun write(key: String, value: ByteArray) { values[key] = value.copyOf() }
    override fun remove(key: String) { values.remove(key) }
}
