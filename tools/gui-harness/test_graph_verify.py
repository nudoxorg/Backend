"""Adversarial checks for the native audit's evidence acceptance rules."""
import copy, hashlib, importlib.util, json, pathlib, re, tempfile, unittest

spec=importlib.util.spec_from_file_location('graph_verify',pathlib.Path(__file__).with_name('graph_verify.py'))
verify=importlib.util.module_from_spec(spec);spec.loader.exec_module(verify)
film_spec=importlib.util.spec_from_file_location('graph_films',pathlib.Path(__file__).with_name('graph_films.py'))
films=importlib.util.module_from_spec(film_spec);film_spec.loader.exec_module(films)

def state(**changes):
    value={'camera':{'x':0.,'y':0.,'w':100.},'world_nodes':3,'find_open':False,'searching':False,
        'exploration':'free','focused':None,'hovered':None,'selected':None,'tour_stop':None,
        'viewport':{'x':96.,'y':60.,'width':480.,'height':400.},'measured_card_bounds':None,
        'moving':False,'prism':None,'pending_motion':0,'drawn':{'edges':0},'pointer':None,
        'query':'','find_scroll':{'x':0.,'y':0.},'find_selection':{'start':0,'end':0}}
    value.update(changes);return value

def frame(at,**changes):return {'at_ms':at,'requested':False,'state':state(**changes),'cpu_ms':1.,'input_cpu_ms':.1,'input_max_ms':.1,'input_events':1}

class EvidenceRules(unittest.TestCase):
    def test_closed_query_requires_measured_zero_scroll_and_valid_selection(self):
        report={'frames':[frame(7600,query='glyph::RelationLabel',find_selection={'start':20,'end':20})]}
        self.assertFalse(verify.state_findings(report,'graph-pinned-world'))
        report['frames'][0]['state']['find_scroll']['x']=-2.5
        self.assertTrue(any('scrolled query' in reason for reason in verify.state_findings(report,'graph-pinned-world')))
        report['frames'][0]['state']['find_open']=True
        self.assertFalse(verify.state_findings(report,'graph-pinned-world'),'native open-field caret scrolling is valid')
        report['frames'][0]['state']['find_selection']['end']=100
        self.assertTrue(any('query bytes' in reason for reason in verify.state_findings(report,'graph-pinned-world')))
        del report['frames'][0]['state']['find_scroll']
        self.assertTrue(any('scroll was absent' in reason for reason in verify.state_findings(report,'graph-pinned-world')))

    def test_same_draw_image_sidecar_compares_semantic_state_and_actual_pixels(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory); image=root/'native-t0.png'
            pixels=Image.new('RGBA',(4,4),(12,34,56,255)); pixels.save(image)
            # The worker duration is preserved in both raw reports, but it is
            # inherently different between the checkpoint and same-draw capture.
            captured={'scene':'graph-pinned-world','time_ms':0,'image':str(image),
                'state':state(discovery_prepare_ms=0.0923),'rgba_sha256':hashlib.sha256(pixels.tobytes()).hexdigest()}
            sidecar=image.with_suffix('.json');sidecar.write_text(json.dumps(captured))
            report={'captures':[{'time_ms':0,'state':state(discovery_prepare_ms=0.058)}]}
            self.assertFalse(verify.native_capture_evidence(root,report,'graph-pinned-world',[0])[1])
            self.assertEqual(captured['state']['discovery_prepare_ms'],0.0923,'raw duration evidence remains intact')
            captured['state']['camera']['x']=1.0;sidecar.write_text(json.dumps(captured))
            self.assertTrue(any('state differs' in reason for reason in verify.native_capture_evidence(root,report,'graph-pinned-world',[0])[1]))
            captured['state']=state(discovery_prepare_ms=0.0923);captured['rgba_sha256']='0'*64;sidecar.write_text(json.dumps(captured))
            self.assertTrue(any('RGBA digest differs' in reason for reason in verify.native_capture_evidence(root,report,'graph-pinned-world',[0])[1]))

    def test_native_capture_missing_or_wrong_timestamp_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(verify.native_capture_evidence(pathlib.Path(directory),{},'graph-pinned-world',[0])[1])
            path=pathlib.Path(directory)/'native-t400.json'
            path.write_text(json.dumps({'time_ms':400,'scene':'graph-pinned-world','state':state(),'image':'absent.png'}))
            reasons=verify.native_capture_evidence(pathlib.Path(directory),{},'graph-pinned-world',[0])[1]
            self.assertIn('native capture timestamps missing, duplicated or unexpected',reasons)
            self.assertTrue(any('PNG is missing' in reason for reason in reasons))

    def test_blank_native_plate_cannot_pass_reading_title_geometry(self):
        from PIL import Image, ImageDraw
        card={'x':10,'y':10,'width':80,'height':60}
        title={'key':'graph-focus-title','content':'Label','x':20,'y':20,'width':40,'height':16}
        capture={'state':state(measured_card_bounds=card),'texts':[title]}
        pixels=Image.new('RGBA',(100,100),(230,235,240,255))
        self.assertTrue(any('no visible painted ink' in reason for reason in verify.painted_title_findings(pixels,capture)))
        ImageDraw.Draw(pixels).rectangle((25,23,28,30),fill=(15,20,30,255))
        self.assertFalse(verify.painted_title_findings(pixels,capture))
        ImageDraw.Draw(pixels).rectangle((25,23,28,30),fill=(230,235,240,255))
        pixels.putpixel((26,25),(229,234,239,255))
        self.assertTrue(verify.painted_title_findings(pixels,capture),'one antialiasing residue cannot prove title paint')

    def test_reading_title_must_exist_in_the_actual_card(self):
        from PIL import Image
        card={'x':10,'y':10,'width':80,'height':60}
        capture={'state':state(measured_card_bounds=card),'texts':[]}
        pixels=Image.new('RGBA',(100,100),(230,235,240,255))
        self.assertTrue(any('title graph-focus-title absent' in reason for reason in verify.painted_title_findings(pixels,capture)))
        capture['texts']=[{'key':'graph-focus-title','content':'Label','x':20,'y':0,'width':40,'height':16}]
        self.assertTrue(any('escapes measured card' in reason for reason in verify.painted_title_findings(pixels,capture)))

    def test_only_worker_wall_time_is_excluded_from_semantic_state_identity(self):
        first={'frames':[frame(7600,discovery_ready=True,discovery_prepare_ms=4.5,preparing_tour=None)]}
        second=copy.deepcopy(first);second['frames'][0]['state']['discovery_prepare_ms']=20.1
        self.assertEqual(verify.stable_motion(first),verify.stable_motion(second))
        self.assertNotEqual(first,second,'raw timing evidence must remain preserved')
        second['frames'][0]['state']['preparing_tour']={'package':1,'at':0}
        self.assertNotEqual(verify.stable_motion(first),verify.stable_motion(second))
        second['frames'][0]['state']['preparing_tour']=None;second['frames'][0]['state']['discovery_ready']=False
        self.assertNotEqual(verify.stable_motion(first),verify.stable_motion(second))

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
            return {'time_ms':at,'state':{'held_chain':None if at<600 else [10,12,13],'reduced_motion':reduced},'tracks':[{
                'key':'graph-chain-road','value':progress,'target':1,'started_ms':240,
                'budget_ms':640,'at_ms':at,'live':live}]}
        report={'captures':[capture(240,0,True),capture(600,.6,True),capture(1600,1,False)]}
        self.assertFalse(verify.road_findings(report))
        restarted=copy.deepcopy(report);restarted['captures'][1]['tracks'][0].update(started_ms=600,value=0)
        self.assertTrue(any('restarted or rewound' in reason for reason in verify.road_findings(restarted)))
        broken=copy.deepcopy(report);broken['captures'][2]['tracks'][0].update(value=.8,live=True)
        self.assertTrue(any('finite arrival' in reason for reason in verify.road_findings(broken)))
        self.assertTrue(verify.road_findings({'captures':report['captures'][1:]}),'missing start cannot be coverage')
        reduced={'captures':[capture(240,1,False,True),capture(600,1,False,True)]}
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

    def test_exact_road_duplicates_are_idempotent_but_conflicts_fail(self):
        track={'key':'graph-chain-road','value':1,'target':1,'started_ms':600,'budget_ms':640,'at_ms':1600,'live':False}
        capture={'time_ms':1600,'state':{'held_chain':[10,12,13],'reduced_motion':True},'tracks':[track,copy.deepcopy(track)]}
        preview={'time_ms':400,'state':{'held_chain':None,'reduced_motion':True},'tracks':[dict(track,at_ms=400)]}
        self.assertFalse(verify.road_findings({'captures':[preview,capture]}))
        capture['tracks'][1]['value']=.5
        self.assertTrue(any('conflicting' in reason for reason in verify.road_findings({'captures':[preview,capture]})))

    def test_footer_cannot_overlap_the_actual_reading_card(self):
        card={'x':108,'y':180,'width':456,'height':260}
        report={'frames':[frame(2000)],'captures':[{'time_ms':2000,'state':{'measured_card_bounds':card},'bounds':[{'key':'graph-where-bounds','x':120,'y':410,'width':150,'height':20}]}]}
        self.assertTrue(any('footer overlaps' in reason for reason in verify.state_findings(report,'graph-pinned-focus')))
        report['captures'][0]['bounds'][0]['y']=150
        self.assertFalse(verify.state_findings(report,'graph-pinned-focus'))

    def test_generated_sweep_is_chronological_and_covers_both_phases(self):
        times=[int(at) for at in re.findall(r'@(\d+)',verify.sweep(480,400))]
        self.assertEqual(len(times),122)
        self.assertEqual(times,sorted(times))
        self.assertEqual((times[0],times[-1]),(0,1936))

if __name__=='__main__':unittest.main()
