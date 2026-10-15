using System.Text.Json;
using Plus.HabboHotel.Items.Wired.Configuration;
// JSONL in; deterministic validation result out. Never print player text/configuration.
var errors = 0;
var total = 0;
string? line;
while ((line = Console.ReadLine()) != null) {
    try {
        using var document = JsonDocument.Parse(line);
        var row = document.RootElement;
        var id = row.GetProperty("item_id").GetUInt32();
        var name = row.GetProperty("box_name").GetString()!;
        var rawConfiguration = row.GetProperty("configuration");
        if (rawConfiguration.GetProperty("SavedState").GetProperty("Snapshots").EnumerateArray()
            .Any(s => s.TryGetProperty("SkipStateRestore", out var skip) && skip.GetBoolean())
            && typeof(WiredFurniSnapshot).GetProperty("SkipStateRestore") == null) {
            Console.Error.WriteLine($"Item {id}: target lacks the Arcturus snapshot skip-state compatibility patch");
            errors++;
        }
        var native = row.GetProperty("configuration").Deserialize<WiredNativeEditorConfiguration>()!;
        if (!WiredBoxRegistry.TryGet(name, out var descriptor)
            || !WiredNativeEditorProjection.TryCompile(id, descriptor, native, out var runtime)) {
            Console.Error.WriteLine($"Rejected Wired item {id}: {name}");
            errors++;
        } else if (row.TryGetProperty("expected_runtime", out var expected) && expected.ValueKind == JsonValueKind.Object) {
            var actual = JsonSerializer.SerializeToElement(runtime);
            foreach (var field in expected.EnumerateObject()) {
                if (!actual.TryGetProperty(field.Name, out var value) || !JsonElement.DeepEquals(value, field.Value)) {
                    Console.Error.WriteLine($"Runtime mismatch for item {id}: {name}, {field.Name}");
                    errors++;
                }
            }
        }
        total++;
    } catch (Exception e) when (e is JsonException or InvalidOperationException or ArgumentException) {
        Console.Error.WriteLine($"Invalid record at line {total + 1}: {e.GetType().Name}");
        errors++;
    }
}
Console.WriteLine(JsonSerializer.Serialize(new { total, errors }));
return errors == 0 ? 0 : 1;
