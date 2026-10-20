import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import javax.inject.Inject

plugins {
    id("com.android.application")
}

abstract class CargoNdkBuild : Exec() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty
}

abstract class ScrcpyServerBuild : DefaultTask() {
    @get:Internal
    abstract val scrcpyDir: DirectoryProperty

    @get:Input
    abstract val windows: Property<Boolean>

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @get:Inject
    abstract val execOperations: ExecOperations

    @get:Inject
    abstract val fileSystemOperations: FileSystemOperations

    @TaskAction
    fun build() {
        val root = scrcpyDir.get().asFile
        if (!root.resolve("server/build.gradle").isFile) {
            throw GradleException(
                "scrcpy submodule is not initialized; run: git submodule update --init --recursive",
            )
        }

        execOperations.exec {
            workingDir(root)
            if (windows.get()) {
                commandLine("cmd", "/c", "gradlew.bat", "assembleRelease")
            } else {
                commandLine("sh", "gradlew", "assembleRelease")
            }
        }

        val apk = root.resolve("server/build/outputs/apk/release/server-release-unsigned.apk")
        if (!apk.isFile) {
            throw GradleException("scrcpy build did not produce $apk")
        }

        fileSystemOperations.copy {
            from(apk)
            into(outputDirectory.get().asFile)
            rename { "scrcpy-server.jar" }
        }
    }
}

val abis = listOf("arm64-v8a", "x86_64")
val minSdkLevel = 24
val rustDir = rootProject.projectDir.parentFile
val rustJniLibsDir = layout.buildDirectory.dir("rust/jniLibs")
val scrcpyRoot = rustDir.resolve("third_party/scrcpy")
val scrcpyAssetsDir = layout.buildDirectory.dir("generated/scrcpyAssets")

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

val scrcpyServer = tasks.register<ScrcpyServerBuild>("scrcpyServer") {
    group = "scrcpy"
    description = "Builds scrcpy-server from the pinned submodule."

    this.scrcpyDir.set(scrcpyRoot)
    windows.set(System.getProperty("os.name").startsWith("Windows"))
    outputDirectory.set(scrcpyAssetsDir)

    inputs.dir(scrcpyRoot.resolve("server/src"))
    inputs.files(
        scrcpyRoot.resolve("server/build.gradle"),
        scrcpyRoot.resolve("build.gradle"),
        scrcpyRoot.resolve("settings.gradle"),
        scrcpyRoot.resolve("gradle.properties"),
        scrcpyRoot.resolve("gradle/wrapper/gradle-wrapper.properties"),
    )
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
        variant.sources.assets?.addGeneratedSourceDirectory(
            scrcpyServer,
            ScrcpyServerBuild::outputDirectory,
        )
    }
}
