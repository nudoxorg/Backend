"""Adversarial checks for the native audit's evidence acceptance rules."""
import copy, importlib.util, pathlib, re, unittest

spec=importlib.util.spec_from_file_location('graph_verify',pathlib.Path(__file__).with_name('graph_verify.py'))
verify=importlib.util.module_from_spec(spec);spec.loader.exec_module(verify)
film_spec=importlib.util.spec_from_file_location('graph_films',pathlib.Path(__file__).with_name('graph_films.py'))
films=importlib.util.module_from_spec(film_spec);film_spec.loader.exec_module(films)

def state(**changes):
    value={'camera':{'x':0.,'y':0.,'w':100.},'world_nodes':3,'find_open':False,'searching':False,
        'exploration':'free','focused':None,'hovered':None,'selected':None,'tour_stop':None,
        'viewport':{'x':96.,'y':60.,'width':480.,'height':400.},'measured_card_bounds':None,
        'moving':False,'prism':None,'pending_motion':0,'drawn':{'edges':0},'pointer':None}
    value.update(changes);return value

def frame(at,**changes):return {'at_ms':at,'requested':False,'state':state(**changes),'cpu_ms':1.,'input_cpu_ms':.1,'input_max_ms':.1,'input_events':1}

class EvidenceRules(unittest.TestCase):
    def test_missing_actual_frames_cannot_pass(self):
        self.assertTrue(verify.state_findings({'frames':[]},'graph-check-idle'))

    def test_focused_subject_is_not_real_hover_coverage(self):
        report={'frames':[frame(900,hovered={'id':1},focused={'id':1},drawn={'edges':8}),frame(7600)],
                'input_events':[{'act':'route graph-hover 1'}]}
        self.assertIn('real hovered symbol never drew edges without focus',verify.state_findings(report,'graph-check-live-hover'))

    def test_invalid_camera_and_out_of_viewport_card_are_rejected(self):
        report={'frames':[frame(7600,camera={'x':float('nan'),'y':0,'w':100},measured_card_bounds={'x':96,'y':450,'width':480,'height':100})]}
        reasons=verify.state_findings(report,'graph-pinned-world')
        self.assertTrue(any('invalid camera' in reason for reason in reasons))
        self.assertTrue(any('escapes actual viewport' in reason for reason in reasons))

    def test_stale_parked_pointer_pick_is_rejected(self):
        report={'frames':[frame(7600,pointer={'x':120,'y':90},prism={'gathered':1},hover_slot=1,pointer_prism_pick=2)]}
        self.assertTrue(any('stale prism pick' in reason for reason in verify.state_findings(report,'graph-pinned-world')))

    def test_enter_must_follow_selected_identity(self):
        gem={'x':130,'y':100,'width':20,'height':20}
        report={'frames':[frame(2200,selected={'id':1}),frame(2400,focused={'id':2},focus_bounds=gem),frame(7600)]}
        self.assertIn('Enter did not follow the actual selected proxy identity',verify.state_findings(report,'graph-check-keyboard'))

    def test_only_measured_durations_are_excluded_from_identity(self):
        first={'frames':[frame(7600)]};second=copy.deepcopy(first)
        second['frames'][0].update(cpu_ms=20.,input_cpu_ms=10.,input_max_ms=5.)
        self.assertEqual(verify.stable_motion(first),verify.stable_motion(second))
        second['frames'][0]['input_events']=0
        self.assertNotEqual(verify.stable_motion(first),verify.stable_motion(second))

    def test_offscreen_text_requires_actual_contained_scroll_reachability(self):
        card={'x':108,'y':180,'width':456,'height':260}
        viewport={'x':120,'y':220,'width':420,'height':200}
        content={'x':120,'y':220,'width':420,'height':1000}
        text={'key':'graph-focus-doc','x':120,'y':900,'width':400,'height':80}
        report={'frames':[frame(2000,focused={'id':0},focus_bounds={'x':130,'y':100,'width':20,'height':20},measured_card_bounds=card)],
                'captures':[{'time_ms':2000,'state':{'measured_card_bounds':card},'texts':[text]}]}
        self.assertTrue(verify.state_findings(report,'graph-pinned-focus'))
        report['captures'][0]['scrolls']=[{'key':'graph-focus-scroll','viewport':viewport,'content':content}]
        self.assertFalse(verify.state_findings(report,'graph-pinned-focus'))
        report['captures'][0]['texts'][0]['width']=500
        self.assertTrue(verify.state_findings(report,'graph-pinned-focus'),'scrolling cannot excuse cross-axis clipping')

    def test_counted_hover_routes_must_preserve_actual_topology(self):
        report={'frames':[frame(900,hovered={'id':1},drawn={'edges':2,'hover_relations':8,'hover_routes':2})]}
        self.assertFalse(verify.live_hover_topology_findings(report,{1:8}))
        report['frames'][0]['state']['drawn']['hover_relations']=7
        self.assertTrue(verify.live_hover_topology_findings(report,{1:8}))
        report['frames'][0]['state']['drawn']['hover_relations']=8
        report['frames'][0]['state']['drawn']['hover_routes']=0
        self.assertTrue(verify.live_hover_topology_findings(report,{1:8}))

    def test_outgoing_hover_requires_finite_opacity_and_exact_topology(self):
        report={'frames':[frame(900,fading_hover={'node':{'id':1},'strength':.5},drawn={'fading_hover_relations':8,'fading_hover_routes':2})]}
        self.assertFalse(verify.live_hover_topology_findings(report,{1:8}))
        report['frames'][0]['state']['drawn']['fading_hover_relations']=7
        self.assertTrue(verify.live_hover_topology_findings(report,{1:8}))
        report['frames'][0]['state']['fading_hover']['strength']=float('nan')
        self.assertTrue(any('opacity' in reason for reason in verify.state_findings(report,'graph-pinned-world')))

    def test_real_road_requires_start_middle_arrival_and_finite_budget(self):
        def capture(at,progress,live,reduced=False):
            return {'time_ms':at,'state':{'held_chain':[10,12,13],'reduced_motion':reduced},'tracks':[{
                'key':'graph-chain-road','value':progress,'target':1,'started_ms':600,
                'budget_ms':640,'at_ms':at,'live':live}]}
        report={'captures':[capture(600,0,True),capture(800,.3,True),capture(1600,1,False)]}
        self.assertFalse(verify.road_findings(report))
        broken=copy.deepcopy(report);broken['captures'][2]['tracks'][0].update(value=.8,live=True)
        self.assertTrue(any('finite arrival' in reason for reason in verify.road_findings(broken)))
        self.assertTrue(verify.road_findings({'captures':report['captures'][1:]}),'missing start cannot be coverage')
        reduced={'captures':[capture(600,1,False,True),capture(800,1,False,True)]}
        self.assertFalse(verify.road_findings(reduced))
        reduced['captures'][0]['tracks'][0].update(value=0,live=True)
        self.assertTrue(any('reduced road' in reason for reason in verify.road_findings(reduced)))

    def test_film_requires_intended_arrival_and_quiet_tail(self):
        report={'frames':[frame(0),frame(7600,focused={'id':1,'name':'RelationLabel'},prism={'gathered':1},focus_bounds={'x':130,'y':100,'width':20,'height':20})]}
        self.assertFalse(films.film_state_findings(report,'graph-flight-a'))
        report['frames'][-1]['state']['focused']['name']='wrong symbol'
        self.assertTrue(films.film_state_findings(report,'graph-flight-a'))
        self.assertTrue(films.film_state_findings({'frames':[]},'graph-flight-a'))

    def test_journey_movie_requires_package_stages_and_exact_world_return(self):
        report={'frames':[frame(0),frame(800,camera={'x':0.,'y':0.,'w':60.}),
            frame(2000,focused={'id':1,'name':'RelationLabel'},prism={'gathered':1},focus_bounds={'x':130,'y':100,'width':20,'height':20}),
            frame(3800,camera={'x':0.,'y':0.,'w':60.}),frame(7600)]}
        self.assertFalse(films.film_state_findings(report,'graph-journey'))
        report['frames'][3]['state']['camera']['w']=100.
        self.assertTrue(any('package view absent' in reason for reason in films.film_state_findings(report,'graph-journey')))
        report['frames'][3]['state']['camera']['w']=60.;report['frames'][-1]['state']['camera']['w']=99.
        self.assertTrue(any('initial world camera' in reason for reason in films.film_state_findings(report,'graph-journey')))

    def test_generated_sweep_is_chronological_and_covers_both_phases(self):
        times=[int(at) for at in re.findall(r'@(\d+)',verify.sweep(480,400))]
        self.assertEqual(len(times),122)
        self.assertEqual(times,sorted(times))
        self.assertEqual((times[0],times[-1]),(0,1936))

if __name__=='__main__':unittest.main()
