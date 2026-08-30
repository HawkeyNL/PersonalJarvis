package com.hawkeynl.jarvis

import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.hawkeynl.jarvis.ui.JarvisApp
import com.hawkeynl.jarvis.ui.JarvisTheme
import com.hawkeynl.jarvis.ui.JarvisViewModel

class MainActivity : FragmentActivity() {
    private val container: AppContainer
        get() = (application as JarvisApplication).container

    private val viewModel: JarvisViewModel by viewModels { JarvisViewModel.factory(container) }
    private var biometricPromptActive = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val state by viewModel.state.collectAsStateWithLifecycle()
            LaunchedEffect(state.locked) {
                if (state.locked) requestBiometricUnlock()
            }
            JarvisTheme {
                JarvisApp(
                    state = state,
                    actions = viewModel,
                    onRequestBiometric = ::requestBiometricUnlock,
                    onInstallUpdate = ::installVerifiedUpdate,
                )
            }
        }
    }

    override fun onStop() {
        super.onStop()
        if (!isChangingConfigurations) viewModel.lockForBackground()
    }

    private fun requestBiometricUnlock() {
        if (biometricPromptActive) return
        biometricPromptActive = true
        container.biometricGate.authenticate(
            activity = this,
            executor = ContextCompat.getMainExecutor(this),
        ) { result ->
            biometricPromptActive = false
            viewModel.retryUnlockResult(result)
        }
    }

    private fun installVerifiedUpdate() {
        viewModel.installerHandoff(container.appUpdates.handOffToPackageInstaller(this))
    }
}
