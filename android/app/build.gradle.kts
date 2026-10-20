import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.OutputDirectory

plugins {
    id("com.android.application")
}

abstract class CargoNdkBuild : Exec() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty
}

val abis = listOf("arm64-v8a", "x86_64")
val minSdkLevel = 24
val rustDir = rootProject.projectDir.parentFile
val rustJniLibsDir = layout.buildDirectory.dir("rust/jniLibs")

android {
    namespace = "id.fahrul.remote"
    compileSdk = 36
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "id.fahrul.remote"
        minSdk = minSdkLevel
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += abis
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

val cargoBuild = tasks.register<CargoNdkBuild>("cargoBuild") {
    group = "rust"
    description = "Compiles the Rust library for Android."

    workingDir(rustDir)
    outputDirectory.set(rustJniLibsDir)
    environment(
        "ANDROID_NDK_HOME",
        androidComponents.sdkComponents.ndkDirectory.get().asFile.absolutePath,
    )
    commandLine(
        buildList {
            add("cargo")
            add("ndk")
            abis.forEach {
                add("-t")
                add(it)
            }
            add("-P")
            add(minSdkLevel.toString())
            add("-o")
            add(outputDirectory.get().asFile.absolutePath)
            add("build")
            add("--lib")
            add("--release")
            add("--locked")
        }
    )

    inputs.dir(rustDir.resolve("src"))
    inputs.files(rustDir.resolve("Cargo.toml"), rustDir.resolve("Cargo.lock"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.jniLibs?.addGeneratedSourceDirectory(
            cargoBuild,
            CargoNdkBuild::outputDirectory,
        )
    }
}
