// ABOUTME: Sends real control frames through generated Kotlin bindings.
// ABOUTME: Exercises typed failures and releases clients without stopping their owner.
import java.nio.file.Files
import java.nio.file.Path
import com.sun.jna.Native
import uniffi.earph_ffi.ClientException
import uniffi.earph_ffi.LocalClient

fun decodeHex(value: String): ByteArray = value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
fun encodeHex(value: ByteArray): String = value.joinToString("") { "%02x".format(it.toInt() and 255) }
fun failure(body: () -> Unit): String {
    try { body() } catch (error: ClientException.Engine) {
        check(error.detail.isNotEmpty())
        return error.code
    }
    error("expected a typed Core failure")
}
fun main(args: Array<String>) {
    check(args.size == 3)
    val commands = Files.readAllLines(Path.of(args[1])).map { it.split('\t', limit = 2) }
    check(commands.size > 1)
    check(Native.POINTER_SIZE > 0)
    check(Path.of(System.getProperty("jnidispatch.path")).toRealPath() == Path.of(args[2]).toRealPath())
    println("private_dispatcher\tverified")
    LocalClient(args[0]).use { client ->
        for (row in commands.dropLast(1)) println(row[0] + "\t" + encodeHex(client.request(decodeHex(row[1]))))
        println("preflight_empty\t!" + failure { client.request(byteArrayOf()) })
        println("preflight_oversized\t!" + failure { client.request(ByteArray(1_048_577)) })
    }
    LocalClient(args[0]).use { client ->
        val row = commands.last()
        println(row[0] + "\t" + encodeHex(client.request(decodeHex(row[1]))))
    }
    println("path_empty\t!" + failure { LocalClient("").use { } })
    println("path_oversized\t!" + failure { LocalClient("é".repeat(31)).use { } })
}
