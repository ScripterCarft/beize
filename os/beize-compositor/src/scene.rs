//! What a frame shows, front to back (`State::render`): the black the
//! desktop fades in from, the pointer where no cursor plane shows it, the
//! shell's panels – above the windows and below them, each with its glass –,
//! the windows with their popups, title bars, round corners and shadows,
//! and the background behind all; and the live glass drawn of what lies
//! behind it.

use std::ops::Range;

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{AsRenderElements, Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::{PopupManager, Space, Window, layer_map_for_output};
use smithay::output::Output as WlOutput;
use smithay::utils::{Logical, Physical, Rectangle, Scale, Size};
use smithay::wayland::shell::wlr_layer::Layer;
use tracing::warn;

use crate::backdrop::{Appearance, Backdrop, LiveGlass};
use crate::corners::{Rounded, Shadow};
use crate::decorations::Decorations;
use crate::render::VkRenderer;

/// In a module of its own: the macro's code names `Result`, which here is
/// the compositor's. Generic over the renderer, as Smithay's concrete form
/// cannot express a frame with two lifetimes (`VkFrame`).
mod element {
    use smithay::backend::renderer::ImportAll;
    use smithay::backend::renderer::ImportMem;
    use smithay::backend::renderer::RendererSuper;
    use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
    use smithay::backend::renderer::element::texture::TextureRenderElement;
    use smithay::render_elements;

    use crate::backdrop::Glass;
    use crate::corners::{Effects, Rounded, Shadow};

    render_elements! {
        /// What a frame draws, front to back: the black the desktop fades
        /// in from (`opening`), the pointer where no cursor plane shows
        /// it, then the surfaces – panels above the windows,
        /// each with its glass, windows each with its popups, title bar,
        /// round corners and shadow, panels below them – and the background
        /// behind all.
        pub Element<R> where R: ImportAll + ImportMem + Effects;
        Veil=SolidColorRenderElement,
        Cursor=MemoryRenderBufferRenderElement<R>,
        Surface=WaylandSurfaceRenderElement<R>,
        Window=Rounded<WaylandSurfaceRenderElement<R>>,
        Bar=Rounded<TextureRenderElement<<R as RendererSuper>::TextureId>>,
        Shadow=Shadow,
        Glass=Glass,
        Wallpaper=TextureRenderElement<<R as RendererSuper>::TextureId>,
    }
}
pub type Element = element::Element<VkRenderer>;

/// The windows on `area`, front to back, each with its popups in front, its
/// content, its title bar, and its shadow, added to `elements`.
pub fn windows(
    renderer: &mut VkRenderer,
    space: &Space<Window>,
    decorations: &mut Decorations,
    area: Rectangle<i32, Logical>,
    elements: &mut Vec<Element>,
) {
    let windows: Vec<_> = space
        .elements()
        .rev()
        .filter_map(|w| Some((w.clone(), space.element_geometry(w)?)))
        .filter(|(_, geometry)| geometry.overlaps(area))
        .collect();
    let mut bars = decorations.elements(renderer, &windows);
    for (window, geometry) in &windows {
        let Some(toplevel) = window.toplevel() else {
            continue;
        };
        let frame = crate::decorations::frame(window, *geometry);
        let rounded = crate::corners::rounded(window, frame).then_some(frame);
        let origin = geometry.loc - window.geometry().loc;
        // Popups in front, never cut by the corners (as Smithay's
        // `Window` places them).
        for (popup, offset) in PopupManager::popups_for_surface(toplevel.wl_surface()) {
            let at = origin + window.geometry().loc + offset - popup.geometry().loc;
            let surfaces = render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                at.to_physical(1),
                Scale::from(1.0),
                1.0,
                Kind::Unspecified,
            );
            elements.extend(surfaces.into_iter().map(Element::Surface));
        }
        let surfaces: Vec<WaylandSurfaceRenderElement<_>> = render_elements_from_surface_tree(
            renderer,
            toplevel.wl_surface(),
            origin.to_physical(1),
            Scale::from(1.0),
            1.0,
            Kind::Unspecified,
        );
        elements.extend(
            surfaces
                .into_iter()
                .map(|s| Element::Window(Rounded::new(s, rounded))),
        );
        if let Some(i) = bars.iter().position(|(w, _)| w == window) {
            let bar = bars.swap_remove(i).1;
            elements.push(Element::Bar(Rounded::new(bar, rounded)));
        }
        if let Some(frame) = rounded {
            elements.push(Element::Shadow(Shadow::new(window, frame)));
        }
    }
}

/// Live glass shows what lies behind it, blurred by the frame where it
/// lies; readied back to front, so glass in front sees that the glass
/// behind it is blurred again. Each panel's keeps its blur (`live`), for a
/// screen of `screen_size`.
pub fn live_glass(
    renderer: &mut VkRenderer,
    live: &mut Vec<LiveGlass>,
    elements: &mut [Element],
    screen_size: Size<i32, Physical>,
    appearance: Appearance,
) {
    for i in (0..elements.len()).rev() {
        let (front, behind) = elements.split_at_mut(i + 1);
        let Element::Glass(glass) = &mut front[i] else {
            continue;
        };
        let Some(below) = glass.live().cloned() else {
            continue;
        };
        let glass_of = match live.iter().position(|l| l.is_for(&below)) {
            Some(j) => &mut live[j],
            None => {
                live.push(LiveGlass::new(below));
                let last = live.len() - 1;
                &mut live[last]
            }
        };
        if let Err(e) = glass_of.update(renderer, behind, glass, screen_size, appearance) {
            warn!("live glass: {e}");
        }
    }
}

/// The black over all of `area` the desktop fades in from (`opening`), of
/// opacity `alpha`.
pub fn veil_over(
    id: &Id,
    commit: CommitCounter,
    area: Rectangle<i32, Logical>,
    alpha: f32,
) -> Element {
    Element::Veil(SolidColorRenderElement::new(
        id.clone(),
        Rectangle::from_size(area.size.to_physical(1)),
        commit,
        [0.0, 0.0, 0.0, alpha],
        Kind::Unspecified,
    ))
}

/// A panel as a frame draws it: its place, whether it lies above the
/// windows, its surfaces front to back and the glass below them.
pub struct PanelFrame {
    pub place: Rectangle<i32, Logical>,
    pub upper: bool,
    pub elements: Vec<Element>,
}

/// The panels, front to back, each with the glass it asked for below it
/// (with a `backdrop`; the menu bar's gives way to the `plate`).
pub fn panels(
    renderer: &mut VkRenderer,
    output: &WlOutput,
    mut backdrop: Option<&mut Backdrop>,
    plate: Option<Rectangle<i32, Logical>>,
) -> Vec<PanelFrame> {
    let map = layer_map_for_output(output);
    let mut drawn = Vec::new();
    // The map lists them back to front.
    for panel in map.layers().rev() {
        let Some(place) = map.layer_geometry(panel) else {
            continue;
        };
        let surfaces: Vec<WaylandSurfaceRenderElement<VkRenderer>> =
            AsRenderElements::<VkRenderer>::render_elements(
                panel,
                renderer,
                place.loc.to_physical(1),
                Scale::from(1.0),
                1.0,
            );
        // The menu bar's own glass gives way to the plate over a fullscreen
        // window.
        let glass = backdrop
            .as_deref_mut()
            .filter(|_| !plate.is_some_and(|band| band.contains_rect(place)))
            .and_then(|b| b.glass(renderer, panel.wl_surface(), place.loc));
        let mut elements: Vec<Element> = surfaces.into_iter().map(Element::Surface).collect();
        elements.extend(glass.map(Element::Glass));
        drawn.push(PanelFrame {
            place,
            upper: matches!(panel.layer(), Layer::Overlay | Layer::Top),
            elements,
        });
    }
    drawn
}

/// Moves the elements of the panels above the windows (`upper`) or below
/// them that are `shown` to the end of `list`; where each went, for
/// `take_back`.
pub fn lend(
    panels: &mut [PanelFrame],
    upper: bool,
    shown: impl Fn(&PanelFrame) -> bool,
    list: &mut Vec<Element>,
) -> Vec<(usize, Range<usize>)> {
    let mut lent = Vec::new();
    for (i, panel) in panels.iter_mut().enumerate() {
        if panel.upper != upper || !shown(panel) {
            continue;
        }
        let start = list.len();
        list.append(&mut panel.elements);
        lent.push((i, start..list.len()));
    }
    lent
}

/// Gives the panels back what `lend` moved to `list`, the last lent first
/// (so the places of the others stay); what lies after is left.
pub fn take_back(
    panels: &mut [PanelFrame],
    list: &mut Vec<Element>,
    lent: Vec<(usize, Range<usize>)>,
) {
    for (i, range) in lent.into_iter().rev() {
        panels[i].elements = list.drain(range).collect();
    }
}
