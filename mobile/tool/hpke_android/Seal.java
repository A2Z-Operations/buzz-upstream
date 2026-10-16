import java.util.Base64;
import xyz.block.buzz.mobile.HostedHpkeBackup;

/** Seals test-only fixture bytes with the actual compiled Android sender. */
public final class Seal {
    private static byte[] hex(String value) {
        byte[] result = new byte[value.length() / 2];
        for (int i = 0; i < result.length; i++) {
            result[i] = (byte) Integer.parseInt(value.substring(i * 2, i * 2 + 2), 16);
        }
        return result;
    }
    public static void main(String[] args) {
        var sealed = HostedHpkeBackup.INSTANCE.seal(
            hex(args[0]), hex(args[1]), hex(args[2]));
        var base64 = Base64.getUrlEncoder().withoutPadding();
        System.out.println(base64.encodeToString(sealed.get("enc")));
        System.out.println(base64.encodeToString(sealed.get("ciphertext")));
    }
}
