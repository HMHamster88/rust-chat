plugins {
    id("com.android.application") version "8.13.2"
}

android {
    namespace = "org.ferroui.spike"
    compileSdk = 35

    defaultConfig {
        applicationId = "org.ferroui.spike"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
}
