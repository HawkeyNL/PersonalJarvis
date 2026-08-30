package com.hawkeynl.jarvis

import android.app.Application
import com.hawkeynl.jarvis.auth.EnrollmentService
import com.hawkeynl.jarvis.chat.ConversationService
import com.hawkeynl.jarvis.network.KtorJarvisApi
import com.hawkeynl.jarvis.security.BiometricGate
import com.hawkeynl.jarvis.security.KeystoreBackedEd25519Identity
import com.hawkeynl.jarvis.storage.AndroidKeystoreSecureValueStore
import com.hawkeynl.jarvis.storage.EndpointSettingsRepository
import com.hawkeynl.jarvis.storage.SessionRepository

class JarvisApplication : Application() {
    val container: AppContainer by lazy { AppContainer(this) }
}

class AppContainer(application: Application) {
    private val secureValues = AndroidKeystoreSecureValueStore(application)
    val settings = EndpointSettingsRepository(application)
    val sessions = SessionRepository(secureValues)
    val identity = KeystoreBackedEd25519Identity(secureValues)
    val api = KtorJarvisApi()
    val enrollment = EnrollmentService(api, identity, sessions)
    val conversations = ConversationService(api, sessions)
    val biometricGate = BiometricGate()
}
