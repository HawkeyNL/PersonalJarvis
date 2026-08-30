package com.hawkeynl.jarvis.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val JarvisColors = darkColorScheme(
    primary = Color(0xFF34F5A0),
    onPrimary = Color(0xFF04140C),
    primaryContainer = Color(0xFF123A29),
    onPrimaryContainer = Color(0xFFC8FFE2),
    secondary = Color(0xFF7DFFC0),
    background = Color(0xFF050A08),
    onBackground = Color(0xFFDFF3E8),
    surface = Color(0xFF0A1410),
    onSurface = Color(0xFFDFF3E8),
    surfaceVariant = Color(0xFF12231B),
    onSurfaceVariant = Color(0xFF91AA9E),
    error = Color(0xFFFFB4AB),
)

@Composable
fun JarvisTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = JarvisColors, content = content)
}
