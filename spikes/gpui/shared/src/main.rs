use spike_shared::showcase::Showcase;

fn main() {
    spike_shared::run("Muxflow GPUI spike", |_, cx| {
        Showcase::new(
            "Muxflow GPUI spike",
            "Inter for chrome text, JetBrains Mono below, on the dark palette.",
            cx,
        )
    });
}
