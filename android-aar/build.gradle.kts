import com.vanniktech.maven.publish.SonatypeHost

plugins {
    id("com.android.library") version "8.8.2"
    id("com.vanniktech.maven.publish") version "0.31.0"
}

val publishVersion = providers.environmentVariable("TODERO_ANDROID_PUBLISH_VERSION")
    .orElse(providers.gradleProperty("VERSION_NAME"))
    .orElse("0.0.0-dev")

val compileSdkVersion = providers.environmentVariable("ANDROID_COMPILE_SDK")
    .orElse("34")
    .map(String::toInt)
val minSdkVersion = providers.environmentVariable("ANDROID_MIN_SDK")
    .orElse("21")
    .map(String::toInt)

version = publishVersion.get()
group = providers.gradleProperty("GROUP").get()

android {
    namespace = "com.shellaia.todero.v3ffi"
    compileSdk = compileSdkVersion.get()

    defaultConfig {
        minSdk = minSdkVersion.get()
        consumerProguardFiles("consumer-rules.pro")
    }

    sourceSets.named("main") {
        jniLibs.srcDir("src/main/jniLibs")
    }

    publishing {
        singleVariant("release") {
            withSourcesJar()
        }
    }

    lint {
        abortOnError = false
    }
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL)
    signAllPublications()
    coordinates(
        providers.gradleProperty("GROUP").get(),
        providers.gradleProperty("POM_ARTIFACT_ID").get(),
        publishVersion.get(),
    )
    pom {
        name.set(providers.gradleProperty("POM_NAME"))
        description.set(providers.gradleProperty("POM_DESCRIPTION"))
        url.set(providers.gradleProperty("POM_URL"))
        licenses {
            license {
                name.set(providers.gradleProperty("POM_LICENSE_NAME"))
                url.set(providers.gradleProperty("POM_LICENSE_URL"))
            }
        }
        scm {
            url.set(providers.gradleProperty("POM_SCM_URL"))
            connection.set(providers.gradleProperty("POM_SCM_CONNECTION"))
            developerConnection.set(providers.gradleProperty("POM_SCM_DEV_CONNECTION"))
        }
        developers {
            developer {
                id.set(providers.gradleProperty("POM_DEVELOPER_ID"))
                name.set(providers.gradleProperty("POM_DEVELOPER_NAME"))
                url.set(providers.gradleProperty("POM_DEVELOPER_URL"))
            }
        }
    }
}
