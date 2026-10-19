// Args: <out.tsv> (f:<float>|p:<hexva>|s:<name>)...
// f: finds f32 little-endian pools and everything that references them.
// p: finds 8-byte little-endian pointers to the given VA.
// s: finds ASCII string hits.
// Every hit is chased two levels: Ghidra references first, then a raw pointer
// scan when a level has no recorded references. Functions are named when the
// from-address sits in one.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.Reference;
import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

public class FloatXrefs extends GhidraScript {
    private long base;
    private PrintWriter out;
    private final Set<Long> printed = new HashSet<>();

    private void note(String tag, String value, Address from, int depth) {
        if (!printed.add(from.getOffset())) {
            return;
        }
        Function f = getFunctionContaining(from);
        Instruction ins = getInstructionAt(from);
        String kind = ins == null ? "data" : "code";
        long rva = f == null ? -1 : f.getEntryPoint().getOffset() - base;
        out.printf("%s\t%s\t%d\t%s\t%x\t%x%n", tag, value, depth, kind, rva,
            from.getOffset() - base);
    }

    private void refsTo(Address target, String tag, String value, int depth)
            throws Exception {
        if (depth > 2) {
            return;
        }
        int seen = 0;
        for (Reference r : getReferencesTo(target)) {
            Address from = r.getFromAddress();
            note(tag, value, from, depth);
            if (getInstructionAt(from) == null && depth + 1 <= 2) {
                refsTo(from, tag, value, depth + 1);
            }
            seen++;
        }
        if (seen == 0) {
            long va = target.getOffset();
            byte[] pointer = new byte[8];
            for (int b = 0; b < 8; b++) {
                pointer[b] = (byte) ((va >>> (8 * b)) & 0xff);
            }
            for (Address holder : scanAll(pointer)) {
                if (holder.compareTo(target) == 0) {
                    continue;
                }
                note(tag, value, holder, depth);
                refsTo(holder, tag, value, depth + 1);
            }
        }
    }

    // Blocks are read in 8 MiB windows with an 8-byte overlap so multi-window
    // hits are not split; unreadable windows are reported, never skipped.
    private List<Address> scanAll(byte[] needle) throws Exception {
        List<Address> hits = new ArrayList<>();
        int window = 64 * 1024;
        for (MemoryBlock block : currentProgram.getMemory().getBlocks()) {
            if (!block.isInitialized()) {
                continue;
            }
            long size = block.getSize();
            for (long off = 0; off < size; off += window - 8) {
                int len = (int) Math.min(window, size - off);
                byte[] bytes = new byte[len];
                try {
                    currentProgram.getMemory()
                        .getBytes(block.getStart().add(off), bytes);
                } catch (Exception e) {
                    println("partial read: " + block.getName() + " +" + off
                        + " (" + e.getMessage() + ")");
                }
                for (int o = 0; o + needle.length <= len; o++) {
                    boolean match = true;
                    for (int b = 0; b < needle.length; b++) {
                        if (bytes[o + b] != needle[b]) {
                            match = false;
                            break;
                        }
                    }
                    if (match) {
                        hits.add(block.getStart().add(off + o));
                    }
                }
            }
        }
        return hits;
    }

    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        base = currentProgram.getImageBase().getOffset();
        try (PrintWriter w = new PrintWriter(args[0], "UTF-8")) {
            out = w;
            for (int i = 1; i < args.length; i++) {
                String arg = args[i];
                String pattern;
                String tag;
                String value;
                if (arg.startsWith("f:")) {
                    int bits = Float.floatToIntBits(Float.parseFloat(arg.substring(2)));
                    pattern = String.format("%02x %02x %02x %02x",
                        bits & 0xff, (bits >>> 8) & 0xff, (bits >>> 16) & 0xff,
                        (bits >>> 24) & 0xff);
                    tag = "float";
                    value = arg.substring(2);
                } else if (arg.startsWith("p:")) {
                    long va = Long.parseUnsignedLong(arg.substring(2), 16);
                    pattern = String.format("%02x %02x %02x %02x %02x %02x %02x %02x",
                        (int) (va & 0xff), (int) ((va >>> 8) & 0xff),
                        (int) ((va >>> 16) & 0xff), (int) ((va >>> 24) & 0xff),
                        (int) ((va >>> 32) & 0xff), (int) ((va >>> 40) & 0xff),
                        (int) ((va >>> 48) & 0xff), (int) ((va >>> 56) & 0xff));
                    tag = "ptr";
                    value = arg.substring(2);
                } else if (arg.startsWith("s:")) {
                    StringBuilder ascii = new StringBuilder();
                    for (char c : arg.substring(2).toCharArray()) {
                        ascii.append(String.format("%02x", (int) c));
                    }
                    pattern = ascii.toString();
                    tag = "str";
                    value = arg.substring(2);
                } else if (arg.startsWith("x:")) {
                    long va = Long.parseUnsignedLong(arg.substring(2), 16);
                    refsTo(toAddr(va), "xref", arg.substring(2), 0);
                    println("xref " + arg.substring(2) + ": done");
                    continue;
                } else {
                    continue;
                }
                byte[] needle = new byte[(pattern.length() + 1) / 3];
                for (int b = 0; b < needle.length; b++) {
                    needle[b] = (byte) Integer.parseInt(
                        pattern.substring(b * 3, b * 3 + 2), 16);
                }
                List<Address> hits = scanAll(needle);
                for (Address hit : hits) {
                    out.printf("%s\t%s\t-1\thit\t-1\t%x%n", tag, value,
                        hit.getOffset() - base);
                    refsTo(hit, tag, value, 0);
                }
                println(tag + " " + value + ": " + hits.size() + " byte hits");
            }
        }
    }
}
