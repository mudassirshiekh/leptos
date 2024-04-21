use crate::{
    location::{Location, Url},
    matching::Routes,
    ChooseView, MatchInterface, MatchNestedRoutes, MatchParams, Params,
    RouteMatchId,
};
use either_of::Either;
use leptos::{component, IntoView};
use or_poisoned::OrPoisoned;
use reactive_graph::{
    computed::ArcMemo,
    owner::{provide_context, use_context, Owner},
    signal::{ArcRwSignal, ArcTrigger},
    traits::{Read, Set, Track, Trigger},
};
use std::{
    borrow::Cow,
    cell::RefCell,
    marker::PhantomData,
    mem,
    rc::Rc,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
};
use tachys::{
    renderer::{dom::Dom, Renderer},
    view::{
        any_view::{AnyView, AnyViewState, IntoAny},
        either::EitherState,
        Mountable, Render, RenderHtml,
    },
};

pub struct Outlet<R> {
    rndr: PhantomData<R>,
}

pub(crate) struct NestedRoutesView<Defs, Fal, R> {
    pub routes: Routes<Defs, R>,
    pub outer_owner: Owner,
    pub url: ArcRwSignal<Url>,
    pub path: ArcMemo<String>,
    pub search_params: ArcMemo<Params>,
    pub base: Option<Cow<'static, str>>,
    pub fallback: Fal,
    pub rndr: PhantomData<R>,
}

pub struct NestedRouteViewState<Fal, R>
where
    Fal: Render<R>,
    R: Renderer + 'static,
{
    outer_owner: Owner,
    url: ArcRwSignal<Url>,
    path: ArcMemo<String>,
    search_params: ArcMemo<Params>,
    outlets: Vec<OutletContext<R>>,
    view: EitherState<Fal::State, AnyViewState<R>, R>,
}

impl<Defs, Fal, R> Render<R> for NestedRoutesView<Defs, Fal, R>
where
    Defs: MatchNestedRoutes<R>,
    Fal: Render<R>,
    R: Renderer + 'static,
{
    type State = NestedRouteViewState<Fal, R>;

    fn build(self) -> Self::State {
        let NestedRoutesView {
            routes,
            outer_owner,
            url,
            path,
            search_params,
            fallback,
            base,
            ..
        } = self;

        let mut outlets = Vec::new();
        let new_match = routes.match_route(&path.read());
        let view = match new_match {
            None => Either::Left(fallback),
            Some(route) => {
                route.build_nested_route(&mut outlets, &outer_owner);
                outer_owner.with(|| {
                    Either::Right(
                        Outlet(OutletProps::builder().build()).into_any(),
                    )
                })
            }
        }
        .build();

        NestedRouteViewState {
            outlets,
            view,
            outer_owner,
            url,
            path,
            search_params,
        }
    }

    fn rebuild(self, state: &mut Self::State) {
        let new_match = self.routes.match_route(&self.path.read());

        // TODO handle fallback => real view, fallback => fallback

        match new_match {
            None => {
                Either::<Fal, AnyView<R>>::Left(self.fallback)
                    .rebuild(&mut state.view);
                state.outlets.clear();
            }
            Some(route) => {
                route.rebuild_nested_route(
                    &mut 0,
                    &mut state.outlets,
                    &self.outer_owner,
                );
            }
        }
    }
}

impl<Defs, Fal, R> RenderHtml<R> for NestedRoutesView<Defs, Fal, R>
where
    Defs: MatchNestedRoutes<R> + Send,
    Fal: RenderHtml<R>,
    R: Renderer + 'static,
{
    type AsyncOutput = Self;

    const MIN_LENGTH: usize = 0; // TODO

    async fn resolve(self) -> Self::AsyncOutput {
        self
    }

    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut tachys::view::Position,
    ) {
        todo!()
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &tachys::hydration::Cursor<R>,
        position: &tachys::view::PositionState,
    ) -> Self::State {
        todo!()
    }
}

type OutletViewFn<R> = Box<dyn FnOnce() -> AnyView<R> + Send>;

#[derive(Debug)]
pub struct OutletContext<R>
where
    R: Renderer,
{
    id: RouteMatchId,
    trigger: ArcTrigger,
    params: ArcRwSignal<Params>,
    owner: Owner,
    tx: Sender<OutletViewFn<R>>,
    rx: Arc<Mutex<Option<Receiver<OutletViewFn<R>>>>>,
}

impl<R> Clone for OutletContext<R>
where
    R: Renderer,
{
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            trigger: self.trigger.clone(),
            params: self.params.clone(),
            owner: self.owner.clone(),
            tx: self.tx.clone(),
            rx: self.rx.clone(),
        }
    }
}

trait AddNestedRoute<R>
where
    R: Renderer,
{
    fn build_nested_route(
        self,
        outlets: &mut Vec<OutletContext<R>>,
        parent: &Owner,
    );

    fn rebuild_nested_route(
        self,
        items: &mut usize,
        outlets: &mut Vec<OutletContext<R>>,
        parent: &Owner,
    );
}

impl<Match, R> AddNestedRoute<R> for Match
where
    Match: MatchInterface<R> + MatchParams,
    R: Renderer + 'static,
{
    fn build_nested_route(
        self,
        outlets: &mut Vec<OutletContext<R>>,
        parent: &Owner,
    ) {
        // each Outlet gets its own owner, so it can inherit context from its parent route,
        // a new owner will be constructed if a different route replaces this one in the outlet,
        // so that any signals it creates or context it provides will be cleaned up
        let owner = parent.child();

        // the params signal can be updated to allow the same outlet to update to changes in the
        // params, even if there's not a route match change
        let params = ArcRwSignal::new(self.to_params().into_iter().collect());

        // the trigger and channel will be used to send new boxed AnyViews to the Outlet;
        // whenever we match a different route, the trigger will be triggered and a new view will
        // be sent through the channel to be rendered by the Outlet
        //
        // combining a trigger and a channel allows us to pass ownership of the view;
        // storing a view in a signal would mean we need to keep a copy stored in the signal and
        // require that we can clone it out
        let trigger = ArcTrigger::new();
        let (tx, rx) = mpsc::channel();

        // add this outlet to the end of the outlet stack used for diffing
        let outlet = OutletContext {
            id: self.as_id(),
            trigger,
            params,
            owner: owner.clone(),
            tx: tx.clone(),
            rx: Arc::new(Mutex::new(Some(rx))),
        };
        outlets.push(outlet.clone());

        // send the initial view through the channel, and recurse through the children
        let (view, child) = self.into_view_and_child();

        tx.send(Box::new({
            let owner = outlet.owner.clone();
            move || owner.with(|| view.choose().into_any())
        }));

        // and share the outlet with the parent via context
        // we share it with the *parent* because the <Outlet/> is rendered in or below the parent
        // wherever it appears, <Outlet/> will look for the closest OutletContext
        parent.with(|| provide_context(outlet));

        // recursively continue building the tree
        // this is important because to build the view, we need access to the outlet
        // and the outlet will be returned from building this child
        if let Some(child) = child {
            child.build_nested_route(outlets, &owner);
        }
    }

    fn rebuild_nested_route(
        self,
        items: &mut usize,
        outlets: &mut Vec<OutletContext<R>>,
        parent: &Owner,
    ) {
        let current = outlets.get_mut(*items);
        match current {
            // if there's nothing currently in the routes at this point, build from here
            None => {
                self.build_nested_route(outlets, parent);
            }
            Some(current) => {
                // a unique ID for each route, which allows us to compare when we get new matches
                // if two IDs are the same, we do not rerender, but only update the params
                // if the IDs are different, we need to replace the remainder of the tree
                let id = self.as_id();

                // whether the route is the same or different, we always need to
                // 1) update the params, and
                // 2) access the view and children
                current
                    .params
                    .set(self.to_params().into_iter().collect::<Params>());
                let (view, child) = self.into_view_and_child();

                // if the IDs don't match, everything below in the tree needs to be swapped:
                // 1) replace this outlet with the next view, with a new owner
                // 2) remove other outlets that are lower down in the match tree
                // 3) build the rest of the list of matched routes, rather than rebuilding,
                //    as all lower outlets needs to be replaced
                if id != current.id {
                    // update the ID of the match at this depth, so that futures rebuilds diff
                    // against the new ID, not the original one
                    current.id = id;

                    // assign a new owner, so that contexts and signals owned by the previous route
                    // in this outlet can be dropped
                    let old_owner =
                        mem::replace(&mut current.owner, parent.child());
                    let owner = current.owner.clone();

                    // send the new view, with the new owner, through the channel to the Outlet,
                    // and notify the trigger so that the reactive view inside the Outlet tracking
                    // the trigger runs again
                    current.tx.send({
                        let owner = owner.clone();
                        Box::new(move || {
                            owner.with(|| view.choose().into_any())
                        })
                    });
                    current.trigger.trigger();

                    // remove all the items lower in the tree
                    // if this match is different, all its children will also be different
                    outlets.truncate(*items + 1);

                    // if this children has matches, then rebuild the lower section of the tree
                    if let Some(child) = child {
                        let mut new_outlets = Vec::new();
                        child.build_nested_route(&mut new_outlets, &owner);
                        outlets.extend(new_outlets);
                    }

                    return;
                }

                // otherwise, just keep rebuilding recursively, checking the remaining routes in
                // the list
                if let Some(child) = child {
                    let owner = current.owner.clone();
                    *items += 1;
                    child.rebuild_nested_route(items, outlets, &owner);
                }
            }
        }
    }
}

impl<Fal, R> Mountable<R> for NestedRouteViewState<Fal, R>
where
    Fal: Render<R>,
    R: Renderer,
{
    fn unmount(&mut self) {
        self.view.unmount();
    }

    fn mount(&mut self, parent: &R::Element, marker: Option<&R::Node>) {
        self.view.mount(parent, marker);
    }

    fn insert_before_this(
        &self,
        parent: &R::Element,
        child: &mut dyn Mountable<R>,
    ) -> bool {
        self.view.insert_before_this(parent, child)
    }
}

#[component]
pub fn Outlet<R>(#[prop(optional)] rndr: PhantomData<R>) -> impl RenderHtml<R>
where
    R: Renderer + 'static,
{
    _ = rndr;
    let ctx = use_context::<OutletContext<R>>()
        .expect("<Outlet/> used without OutletContext being provided.");
    let OutletContext {
        id,
        trigger,
        params,
        owner,
        tx,
        rx,
    } = ctx;
    let rx = rx.lock().or_poisoned().take().expect(
        "Tried to render <Outlet/> but could not find the view receiver. Are \
         you using the same <Outlet/> twice?",
    );
    move || {
        trigger.track();

        rx.try_recv().map(|view| view()).unwrap()
    }
}