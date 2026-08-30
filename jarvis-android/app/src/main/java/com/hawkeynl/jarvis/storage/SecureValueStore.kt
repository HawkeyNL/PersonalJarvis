package com.hawkeynl.jarvis.storage

interface SecureValueStore {
    fun exists(key: String): Boolean
    fun read(key: String): ByteArray?
    fun write(key: String, value: ByteArray)
    fun remove(key: String)
}

class SecureStorageException(message: String, cause: Throwable? = null) : Exception(message, cause)
